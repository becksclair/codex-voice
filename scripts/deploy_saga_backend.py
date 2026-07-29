#!/usr/bin/env python3
"""Deploy one published Codex Voice backend artifact to Saga.

This is deliberately a backend-only coordinator.  It invokes the established
central Ansible selected-service installer, restarts only Saga's backend unit,
and delegates live proof to the canonical Saga/producer acceptance harnesses.
Protected controller inputs are referenced as files and are never emitted.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request


COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
ENV_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
CENTRAL_HOMELAB_COMMIT = "674eb3d15c496dc1ce6d6b4408bfe33497092226"
SAGA_HOMELAB_COMMIT = "1b4c308e4c82570b4715c4e4951e899f58c7d098"
STAGES = {
    "preflight",
    "download",
    "artifact-validation",
    "selected-service-install",
    "backend-activation",
    "installed-acceptance",
    "complete",
}


class DeployError(RuntimeError):
    """A bounded, non-secret deployment contract failure."""


def _validate_pinned_archive_member(member: tarfile.TarInfo, destination: Path) -> None:
    path = PurePosixPath(member.name)
    if path.is_absolute() or ".." in path.parts:
        raise DeployError("pinned repository archive contains an unsafe member")
    try:
        tarfile.data_filter(member, destination)
    except tarfile.FilterError as error:
        raise DeployError("pinned repository archive contains an unsafe member") from error


def write_stage(path: Path, stage: str) -> None:
    if stage not in STAGES:
        raise DeployError(f"invalid deployment stage: {stage}")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(stage + "\n", encoding="utf-8")
    temporary.chmod(0o600)
    temporary.replace(path)


def require_protected_regular_file(path: Path, label: str) -> None:
    if not path.is_absolute():
        raise DeployError(f"{label} path must be absolute")
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise DeployError(f"{label} is not provisioned") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise DeployError(f"{label} must be a regular non-symlink file")
    if stat.S_IMODE(metadata.st_mode) != 0o600:
        raise DeployError(f"{label} must have mode 0600")
    if metadata.st_uid != os.getuid():
        raise DeployError(f"{label} must be owned by the deployment runner user")


def configured_provider_environment_names(
    config: dict[str, object], actual_names: set[str]
) -> set[str]:
    providers = config.get("providers")
    if not isinstance(providers, dict) or not providers:
        raise DeployError("Codex Voice config declares no providers")
    advanced = config.get("advanced", {})
    advanced_providers = (
        advanced.get("providers", {}) if isinstance(advanced, dict) else {}
    )
    if not isinstance(advanced_providers, dict):
        raise DeployError("Codex Voice config has an invalid advanced provider shape")

    defaults = {
        "google": ("GEMINI_API_KEY", "GOOGLE_API_KEY"),
        "elevenlabs": ("ELEVENLABS_API_KEY", "ELEVEN_API_KEY"),
    }
    selected_names: set[str] = set()
    for provider_name in providers:
        if provider_name not in defaults:
            raise DeployError("Codex Voice config declares an unsupported provider")
        override = advanced_providers.get(provider_name, {})
        if not isinstance(override, dict):
            raise DeployError("Codex Voice config has an invalid provider override")
        selected = override.get("apiKeyEnv")
        if selected is not None and (
            not isinstance(selected, str) or not ENV_NAME_RE.fullmatch(selected)
        ):
            raise DeployError("config contains an invalid apiKeyEnv name")
        candidates = (selected,) if selected else defaults[provider_name]
        configured = [name for name in candidates if name in actual_names]
        if len(configured) != 1:
            raise DeployError(
                "provider environment must select exactly one key name per provider"
            )
        selected_names.add(configured[0])
    return selected_names


def provider_environment_names(path: Path) -> set[str]:
    names: set[str] = set()
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line.removeprefix("export ").strip()
        name, separator, value = line.partition("=")
        if not separator or not ENV_NAME_RE.fullmatch(name) or not value:
            raise DeployError("provider environment contains an invalid assignment")
        if name in names:
            raise DeployError("provider environment contains a duplicate assignment")
        names.add(name)
    return names


def validate_protected_inputs(config_path: Path, provider_env_path: Path) -> None:
    require_protected_regular_file(config_path, "Codex Voice config source")
    require_protected_regular_file(provider_env_path, "provider environment source")
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise DeployError(
            "Codex Voice config source is not valid UTF-8 JSON"
        ) from error
    if not isinstance(config, dict) or config.get("version") != 1:
        raise DeployError("Codex Voice config source must use schema version 1")
    actual_names = provider_environment_names(provider_env_path)
    selected_names = configured_provider_environment_names(config, actual_names)
    if actual_names != selected_names:
        raise DeployError(
            "provider environment names do not exactly match the active config"
        )


def validate_server_url(value: str) -> str:
    parsed = urllib.parse.urlparse(value)
    if parsed.scheme != "https" or not parsed.netloc or parsed.path not in ("", "/"):
        raise DeployError("Gitea server URL must be one HTTPS origin")
    if (
        parsed.params
        or parsed.query
        or parsed.fragment
        or parsed.username
        or parsed.password
    ):
        raise DeployError(
            "Gitea server URL must not contain credentials or extra components"
        )
    return value.rstrip("/")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_sidecar(text: str, archive_name: str) -> str:
    match = re.fullmatch(r"([0-9a-f]{64})  ([^\r\n]+)\n?", text)
    if not match or match.group(2) != archive_name:
        raise DeployError("published checksum sidecar has an invalid identity")
    return match.group(1)


def download_file(url: str, destination: Path) -> None:
    request = urllib.request.Request(
        url, headers={"User-Agent": "codex-voice-deploy/1"}
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            if response.status != 200:
                raise DeployError("published package read-back did not return HTTP 200")
            with destination.open("xb") as output:
                shutil.copyfileobj(response, output)
    except (OSError, urllib.error.URLError) as error:
        raise DeployError("published package read-back failed") from error
    destination.chmod(0o600)


def require_git_commit(repository: Path, commit: str, label: str) -> None:
    if not repository.is_absolute() or not (repository / ".git").exists():
        raise DeployError(f"{label} repository is unavailable")
    result = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", f"{commit}^{{commit}}"],
        check=True,
        capture_output=True,
        text=True,
    )
    if result.stdout.strip() != commit:
        raise DeployError(f"{label} commit identity mismatch")


def require_exact_clean_checkout(repository: Path, commit: str) -> None:
    require_git_commit(repository, commit, "producer checkout")
    head = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    status = subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    if head != commit or status:
        raise DeployError("producer checkout is not the exact clean source commit")


def extract_git_commit(repository: Path, commit: str, destination: Path) -> None:
    archive_path = destination.parent / (destination.name + ".tar")
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "archive",
            "--format=tar",
            "-o",
            str(archive_path),
            commit,
        ],
        check=True,
    )
    destination.mkdir(mode=0o700)
    with tarfile.open(archive_path, "r:") as archive:
        for member in archive.getmembers():
            _validate_pinned_archive_member(member, destination)
        archive.extractall(destination, filter="data")
    archive_path.unlink()


def run_command(argv: list[str], *, environment: dict[str, str] | None = None) -> None:
    subprocess.run(argv, check=True, env=environment)


def deployment_vars(
    *,
    source_commit: str,
    archive_url: str,
    archive_sha256: str,
    saga_source: Path,
    config_source: Path,
    provider_env_source: Path,
) -> dict[str, object]:
    return {
        "artifact_sha256": archive_sha256,
        "artifact_url": archive_url,
        "external_inputs": [
            {
                "destination": "/home/ubuntu/.config/codex-voice/config.json",
                "group": "ubuntu",
                "mode": "0600",
                "owner": "ubuntu",
                "parent_group": "ubuntu",
                "parent_mode": "0700",
                "parent_owner": "ubuntu",
                "requirement_id": "codex-voice-config",
                "source": str(config_source),
            },
            {
                "destination": "/srv/services-state/codex-voice/codex-voice.env",
                "group": "ubuntu",
                "mode": "0600",
                "owner": "ubuntu",
                "parent_group": "ubuntu",
                "parent_mode": "0700",
                "parent_owner": "ubuntu",
                "requirement_id": "codex-voice-provider-environment",
                "source": str(provider_env_source),
            },
        ],
        "saga_homelab_source": str(saga_source),
        "service_id": "codex-voice",
        "source_commit": source_commit,
    }


def deploy(args: argparse.Namespace) -> None:
    source_commit = args.source_commit
    if not COMMIT_RE.fullmatch(source_commit):
        raise DeployError("source commit must be 40 lowercase hexadecimal characters")
    server_url = validate_server_url(args.gitea_server_url)
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", args.package_owner):
        raise DeployError("package owner has an invalid shape")

    stage_path = args.stage_file.resolve()
    write_stage(stage_path, "preflight")
    require_exact_clean_checkout(args.producer_root, source_commit)
    validate_protected_inputs(args.config_source, args.provider_env_source)
    require_git_commit(
        args.central_repository, CENTRAL_HOMELAB_COMMIT, "central homelab"
    )
    require_git_commit(args.saga_repository, SAGA_HOMELAB_COMMIT, "Saga homelab")

    archive_name = f"codex-voice-linux-amd64-{source_commit}.tar.gz"
    package_url = (
        f"{server_url}/api/packages/{args.package_owner}/generic/codex-voice/"
        f"{source_commit}"
    )
    archive_url = f"{package_url}/{archive_name}"

    with tempfile.TemporaryDirectory(prefix="codex-voice-saga-deploy-") as directory:
        workspace = Path(directory)
        archive_path = workspace / archive_name
        sidecar_path = workspace / f"{archive_name}.sha256"
        central_source = workspace / "central-homelab"
        saga_source = workspace / "saga-homelab"

        write_stage(stage_path, "download")
        download_file(archive_url, archive_path)
        download_file(f"{archive_url}.sha256", sidecar_path)
        expected_sha256 = parse_sidecar(
            sidecar_path.read_text(encoding="utf-8"), archive_name
        )
        if (
            not SHA256_RE.fullmatch(expected_sha256)
            or sha256_file(archive_path) != expected_sha256
        ):
            raise DeployError("published archive does not match its immutable checksum")
        args.archive_sha_file.write_text(expected_sha256 + "\n", encoding="utf-8")
        args.archive_sha_file.chmod(0o600)

        extract_git_commit(
            args.central_repository, CENTRAL_HOMELAB_COMMIT, central_source
        )
        extract_git_commit(args.saga_repository, SAGA_HOMELAB_COMMIT, saga_source)

        write_stage(stage_path, "artifact-validation")
        run_command(
            [
                "python3",
                str(args.producer_root / "scripts/validate_runtime_artifact.py"),
                str(archive_path),
                source_commit,
            ]
        )

        variables_path = workspace / "deployment-vars.json"
        variables_path.write_text(
            json.dumps(
                deployment_vars(
                    source_commit=source_commit,
                    archive_url=archive_url,
                    archive_sha256=expected_sha256,
                    saga_source=saga_source,
                    config_source=args.config_source,
                    provider_env_source=args.provider_env_source,
                ),
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        variables_path.chmod(0o600)

        ansible_playbook = args.central_repository / "ops/.venv/bin/ansible-playbook"
        if not ansible_playbook.is_file() or not os.access(ansible_playbook, os.X_OK):
            raise DeployError("central locked Ansible executable is unavailable")
        ansible_environment = os.environ.copy()
        ansible_environment["ANSIBLE_CONFIG"] = str(
            central_source / "ansible/ansible.cfg"
        )

        write_stage(stage_path, "selected-service-install")
        run_command(
            [
                str(ansible_playbook),
                "-i",
                str(central_source / "generated/saga/inventory.json"),
                str(central_source / "ansible/playbooks/saga-service-install.yml"),
                "--extra-vars",
                '{"homelab_root":""}',
                "--extra-vars",
                f"@{variables_path}",
            ],
            environment=ansible_environment,
        )

        write_stage(stage_path, "backend-activation")
        run_command(
            [
                "ssh",
                "-o",
                "BatchMode=yes",
                "--",
                args.ssh_target,
                "sudo -n systemctl daemon-reload && "
                "sudo -n systemctl enable codex-voice.service && "
                "sudo -n systemctl restart codex-voice.service",
            ]
        )

        write_stage(stage_path, "installed-acceptance")
        run_command(
            [
                "python3",
                str(saga_source / "scripts/validate-codex-voice-private-canary.py"),
                "--archive",
                str(archive_path),
                "--source-commit",
                source_commit,
                "--archive-sha256",
                expected_sha256,
                "--producer-validator",
                str(args.producer_root / "scripts/validate_runtime_artifact.py"),
                "--producer-acceptance",
                str(args.producer_root / "scripts/accept_installed_service.py"),
                "--ssh-target",
                args.ssh_target,
            ]
        )

    write_stage(stage_path, "complete")
    print(f"Saga backend deployment accepted for source commit {source_commit}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--gitea-server-url", required=True)
    parser.add_argument("--package-owner", required=True)
    parser.add_argument("--stage-file", required=True, type=Path)
    parser.add_argument("--archive-sha-file", required=True, type=Path)
    parser.add_argument("--producer-root", required=True, type=Path)
    parser.add_argument("--central-repository", required=True, type=Path)
    parser.add_argument("--saga-repository", required=True, type=Path)
    parser.add_argument("--config-source", required=True, type=Path)
    parser.add_argument("--provider-env-source", required=True, type=Path)
    parser.add_argument("--ssh-target", default="saga")
    return parser.parse_args()


def main() -> int:
    try:
        deploy(parse_args())
    except (DeployError, subprocess.CalledProcessError) as error:
        print(
            f"Codex Voice Saga backend deployment failed: {error}", file=os.sys.stderr
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
