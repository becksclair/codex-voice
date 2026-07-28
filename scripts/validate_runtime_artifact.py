#!/usr/bin/env python3
"""Validate the immutable Codex Voice linux-amd64 runtime artifact contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import socket
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path, PurePosixPath


SCHEMA_VERSION = "saga-binary-service-artifact/v1"
SERVICE_ID = "codex-voice"
PLATFORM = "linux-amd64"
ARCHIVE_PREFIX = f"{SERVICE_ID}-{PLATFORM}-"
FULL_SHA = re.compile(r"[0-9a-f]{40}")
HASHED_ASSET = re.compile(r"^/web/assets/[^/?#]+-[A-Za-z0-9_-]{8,}\.[^/?#]+$")
EXPECTED_MEMBERS = {
    SERVICE_ID,
    f"{SERVICE_ID}/artifact.json",
    f"{SERVICE_ID}/{SERVICE_ID}",
}


class ValidationError(ValueError):
    pass


def expected_manifest(source_commit: str) -> dict[str, object]:
    return {
        "executable": SERVICE_ID,
        "health_path": "/healthz",
        "platform": PLATFORM,
        "schema_version": SCHEMA_VERSION,
        "service_id": SERVICE_ID,
        "source_commit": source_commit,
    }


def canonical_manifest_bytes(source_commit: str) -> bytes:
    return (
        json.dumps(expected_manifest(source_commit), sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _validate_gzip_header(archive: Path) -> None:
    with archive.open("rb") as handle:
        header = handle.read(10)
    if len(header) != 10 or header[:3] != b"\x1f\x8b\x08":
        raise ValidationError("archive is not a gzip stream")
    flags = header[3]
    mtime = struct.unpack("<I", header[4:8])[0]
    if flags != 0 or mtime != 0:
        raise ValidationError("gzip header must have no optional fields and mtime=0")


def _validate_member_metadata(member: tarfile.TarInfo) -> None:
    path = PurePosixPath(member.name)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise ValidationError(f"unsafe member path: {member.name}")
    if member.uid != 0 or member.gid != 0 or member.mtime != 0:
        raise ValidationError(f"non-deterministic metadata on {member.name}")
    if member.uname or member.gname:
        raise ValidationError(f"owner names are not allowed on {member.name}")


def _validate_elf_amd64(binary: bytes) -> None:
    if len(binary) < 20 or binary[:4] != b"\x7fELF":
        raise ValidationError("codex-voice is not an ELF executable")
    if binary[4] != 2 or binary[5] != 1:
        raise ValidationError("codex-voice must be a little-endian ELF64 executable")
    if struct.unpack("<H", binary[18:20])[0] != 62:
        raise ValidationError("codex-voice ELF machine must be x86-64")


def validate_artifact(archive: Path, source_commit: str) -> None:
    if not FULL_SHA.fullmatch(source_commit):
        raise ValidationError("source commit must be a lowercase 40-character SHA")
    expected_name = f"{ARCHIVE_PREFIX}{source_commit}.tar.gz"
    if archive.name != expected_name:
        raise ValidationError(f"archive filename must be {expected_name}")
    if not archive.is_file():
        raise ValidationError(f"archive does not exist: {archive}")

    sidecar = archive.with_name(f"{archive.name}.sha256")
    expected_sidecar = f"{sha256_file(archive)}  {archive.name}\n"
    try:
        actual_sidecar = sidecar.read_text()
    except FileNotFoundError as error:
        raise ValidationError(f"missing checksum sidecar: {sidecar.name}") from error
    if actual_sidecar != expected_sidecar:
        raise ValidationError("checksum sidecar is missing, malformed, or incorrect")

    _validate_gzip_header(archive)
    try:
        with tarfile.open(archive, "r:gz") as bundle:
            members = bundle.getmembers()
            names = [member.name.rstrip("/") for member in members]
            if len(names) != len(set(names)):
                raise ValidationError("archive contains duplicate member names")
            if set(names) != EXPECTED_MEMBERS:
                raise ValidationError(f"archive members must be exactly {sorted(EXPECTED_MEMBERS)}")
            by_name = {member.name.rstrip("/"): member for member in members}
            for member in members:
                _validate_member_metadata(member)

            root = by_name[SERVICE_ID]
            manifest_member = by_name[f"{SERVICE_ID}/artifact.json"]
            binary_member = by_name[f"{SERVICE_ID}/{SERVICE_ID}"]
            if not root.isdir() or root.mode != 0o755:
                raise ValidationError("archive root must be a mode-0755 directory")
            if not manifest_member.isfile() or manifest_member.mode != 0o644:
                raise ValidationError("artifact.json must be a mode-0644 regular file")
            if not binary_member.isfile() or binary_member.mode != 0o755:
                raise ValidationError("codex-voice must be a mode-0755 regular file")

            manifest_handle = bundle.extractfile(manifest_member)
            binary_handle = bundle.extractfile(binary_member)
            if manifest_handle is None or binary_handle is None:
                raise ValidationError("regular artifact members could not be read")
            manifest = manifest_handle.read(65537)
            binary_prefix = binary_handle.read(20)
    except (tarfile.TarError, EOFError, OSError) as error:
        raise ValidationError(f"invalid tar archive: {error}") from error

    if len(manifest) > 65536:
        raise ValidationError("artifact.json exceeds 64 KiB")
    if manifest != canonical_manifest_bytes(source_commit):
        raise ValidationError("artifact.json is not the exact canonical manifest")
    _validate_elf_amd64(binary_prefix)


def _http_get(url: str) -> tuple[bytes, object]:
    request = urllib.request.Request(url, headers={"User-Agent": "codex-voice-artifact-validator"})
    with urllib.request.urlopen(request, timeout=2) as response:
        return response.read(), response.headers


def _available_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def smoke_environment(runtime: Path) -> dict[str, str]:
    environment = {
        key: os.environ[key]
        for key in ("PATH", "LD_LIBRARY_PATH", "LANG", "LC_ALL")
        if key in os.environ
    }
    environment.update(
        {
            "HOME": str(runtime / "home"),
            "CODEX_HOME": str(runtime / "codex"),
            "XDG_CONFIG_HOME": str(runtime / "config"),
            "XDG_STATE_HOME": str(runtime / "state"),
            "XDG_DATA_HOME": str(runtime / "data"),
            "XDG_CACHE_HOME": str(runtime / "cache"),
            "XDG_RUNTIME_DIR": str(runtime / "run"),
            "TMPDIR": str(runtime / "tmp"),
            "CODEX_VOICE_TRANSCRIBER_TOKEN": "artifact-smoke-not-secret",
        }
    )
    return environment


def smoke_artifact(archive: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="codex-voice-artifact-smoke-") as temp:
        root = Path(temp)
        with tarfile.open(archive, "r:gz") as bundle:
            bundle.extractall(root, filter="data")
        binary = root / SERVICE_ID / SERVICE_ID

        version = subprocess.run(
            [binary, "--version"],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
        if not re.fullmatch(r"codex-voice \S+", version):
            raise ValidationError(f"unexpected --version output: {version!r}")

        port = _available_port()
        runtime = root / "runtime"
        environment = smoke_environment(runtime)
        for name in ("home", "codex", "config", "state", "data", "cache", "run", "tmp"):
            (runtime / name).mkdir(parents=True, exist_ok=True)
        process = subprocess.Popen(
            [binary, "server", "--bind", f"127.0.0.1:{port}"],
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        base_url = f"http://127.0.0.1:{port}"
        try:
            deadline = time.monotonic() + 15
            health = None
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    diagnostics = (process.stderr.read() if process.stderr else "").strip()[-2000:]
                    raise ValidationError(f"smoke server exited early: {diagnostics}")
                try:
                    health_body, _ = _http_get(f"{base_url}/healthz")
                    health = json.loads(health_body)
                    break
                except (OSError, urllib.error.URLError, json.JSONDecodeError):
                    time.sleep(0.1)
            if health is None:
                raise ValidationError("smoke server did not become healthy within 15 seconds")
            if health.get("ok") is not True or health.get("capabilities", {}).get("desktop") is not True:
                raise ValidationError("healthz did not prove a non-stub embedded desktop PWA")

            index, index_headers = _http_get(f"{base_url}/web")
            index_text = index.decode("utf-8")
            if "Web UI not built" in index_text:
                raise ValidationError("embedded web index is the build-time stub")
            assets = sorted(set(re.findall(r'''(?:src|href)=["']([^"']+)["']''', index_text)))
            hashed_assets = [asset for asset in assets if HASHED_ASSET.fullmatch(asset)]
            if not any(asset.endswith(".js") for asset in hashed_assets):
                raise ValidationError("embedded web index has no content-hashed JavaScript asset")
            if index_headers.get("Cache-Control") != "no-cache":
                raise ValidationError("embedded web index must be served with no-cache")
            for asset in hashed_assets:
                body, headers = _http_get(f"{base_url}{asset}")
                if not body:
                    raise ValidationError(f"embedded web asset is empty: {asset}")
                if headers.get("Cache-Control") != "public, max-age=31536000, immutable":
                    raise ValidationError(f"embedded web asset lacks immutable caching: {asset}")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("source_commit")
    parser.add_argument("--smoke", action="store_true", help="run the extracted binary and embedded PWA")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        validate_artifact(args.archive, args.source_commit)
        if args.smoke:
            smoke_artifact(args.archive)
    except (ValidationError, subprocess.SubprocessError, OSError) as error:
        print(f"artifact validation failed: {error}", file=sys.stderr)
        return 1
    print(f"validated {args.archive.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
