#!/usr/bin/env python3
"""Accept an installed Codex Voice service through a loopback/private HTTP path."""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path


ATTESTATION_SCHEMA = "codex-voice-installed-service/v1"
ACCEPTANCE_SCHEMA = "codex-voice-installed-acceptance/v1"
SERVICE_ID = "codex-voice"
FULL_SHA = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
INSTANCE_ID = re.compile(r"[0-9a-f]{32}")
VERSION = re.compile(r"codex-voice \S+")
HASHED_ASSET = re.compile(r"^/web/assets/[^/?#]+-[A-Za-z0-9_-]{8,}\.[^/?#]+$")
CANARY_TEXT = "The private canary number is seven."
MAX_JSON_BYTES = 256 * 1024
MAX_INDEX_BYTES = 2 * 1024 * 1024
MAX_ASSET_BYTES = 8 * 1024 * 1024
MAX_AUDIO_BYTES = 32 * 1024 * 1024
MAX_PWA_ASSETS = 64
MAX_PWA_MANIFESTS = 4
MAX_PWA_ICON_BYTES = 2 * 1024 * 1024
PWA_DEADLINE_SECONDS = 60
ATTESTATION_KEYS = {
    "schema_version",
    "service_id",
    "source_commit",
    "artifact_sha256",
    "artifact_binary_sha256",
    "installed_binary_sha256",
    "version",
    "unit",
    "unit_user",
    "active_state",
    "sub_state",
    "listener",
    "service_instance_id",
    "config_sha256",
    "config_ready",
    "provider_environment_ready",
    "codex_auth_ready",
}


class AcceptanceError(ValueError):
    pass


def _require_digest(value: object, label: str) -> str:
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise AcceptanceError(f"{label} must be a lowercase SHA-256 digest")
    return value


def validate_base_url(base_url: str) -> str:
    parsed = urllib.parse.urlsplit(base_url)
    if (
        parsed.scheme != "http"
        or parsed.hostname not in {"127.0.0.1", "localhost"}
        or parsed.port is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in {"", "/"}
        or parsed.query
        or parsed.fragment
    ):
        raise AcceptanceError(
            "base URL must be an explicit http://127.0.0.1:<port> or localhost SSH-forward/loopback URL"
        )
    return f"http://{parsed.hostname}:{parsed.port}"


def load_attestation(
    path: Path, source_commit: str, artifact_sha256: str
) -> dict[str, object]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise AcceptanceError(
            f"could not read installed attestation: {error.strerror}"
        ) from error
    if len(raw) > MAX_JSON_BYTES:
        raise AcceptanceError("installed attestation exceeds 256 KiB")
    try:
        attestation = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AcceptanceError("installed attestation is not valid JSON") from error
    if not isinstance(attestation, dict) or set(attestation) != ATTESTATION_KEYS:
        raise AcceptanceError(
            "installed attestation does not have the exact v1 field set"
        )
    if attestation["schema_version"] != ATTESTATION_SCHEMA:
        raise AcceptanceError("installed attestation has the wrong schema version")
    if attestation["service_id"] != SERVICE_ID:
        raise AcceptanceError("installed attestation has the wrong service ID")
    if attestation["source_commit"] != source_commit:
        raise AcceptanceError(
            "installed source commit does not match the requested artifact"
        )
    if attestation["artifact_sha256"] != artifact_sha256:
        raise AcceptanceError(
            "installed archive digest does not match the requested artifact"
        )
    artifact_binary = _require_digest(
        attestation["artifact_binary_sha256"], "artifact binary digest"
    )
    installed_binary = _require_digest(
        attestation["installed_binary_sha256"], "installed binary digest"
    )
    if artifact_binary != installed_binary:
        raise AcceptanceError(
            "installed binary digest does not match the validated artifact binary"
        )
    _require_digest(attestation["config_sha256"], "config fingerprint")
    if not isinstance(
        attestation["service_instance_id"], str
    ) or not INSTANCE_ID.fullmatch(attestation["service_instance_id"]):
        raise AcceptanceError(
            "service instance ID must be 32 lowercase hexadecimal characters"
        )
    if not isinstance(attestation["version"], str) or not VERSION.fullmatch(
        attestation["version"]
    ):
        raise AcceptanceError("installed binary has unexpected --version output")
    expected = {
        "unit": "codex-voice.service",
        "unit_user": "ubuntu",
        "active_state": "active",
        "sub_state": "running",
        "listener": "127.0.0.1:3845",
        "config_ready": True,
        "provider_environment_ready": True,
        "codex_auth_ready": True,
    }
    for key, value in expected.items():
        if attestation[key] != value:
            raise AcceptanceError(f"installed attestation did not prove required {key}")
    return attestation


def _request(
    url: str, *, data: bytes | None = None, headers: dict[str, str] | None = None
) -> urllib.request.Request:
    request_headers = {"User-Agent": "codex-voice-installed-acceptance"}
    request_headers.update(headers or {})
    return urllib.request.Request(url, data=data, headers=request_headers)


class _RejectRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(
        self,
        _request: urllib.request.Request,
        _file_pointer: object,
        _code: int,
        _message: str,
        _headers: object,
        _new_url: str,
    ) -> None:
        return None


HTTP_OPENER = urllib.request.build_opener(
    urllib.request.ProxyHandler({}), _RejectRedirects
)


def _read_response(
    request: urllib.request.Request, limit: int, *, deadline: float | None = None
) -> tuple[bytes, object]:
    remaining = 90.0 if deadline is None else deadline - time.monotonic()
    if remaining <= 0:
        raise AcceptanceError(f"request deadline expired before {request.selector}")
    try:
        with HTTP_OPENER.open(request, timeout=min(90.0, remaining)) as response:
            body = bytearray()
            while True:
                if deadline is not None:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise AcceptanceError(
                            f"request deadline expired while reading {request.selector}"
                        )
                    try:
                        response.fp.raw._sock.settimeout(remaining)
                    except (AttributeError, OSError):
                        pass
                chunk = response.read1(min(64 * 1024, limit + 1 - len(body)))
                if not chunk:
                    break
                body.extend(chunk)
                if len(body) > limit:
                    raise AcceptanceError(
                        f"response from {request.selector} exceeded its bounded limit"
                    )
            return bytes(body), response.headers
    except urllib.error.HTTPError as error:
        raise AcceptanceError(f"HTTP {error.code} from {request.selector}") from error
    except urllib.error.URLError as error:
        reason = type(error.reason).__name__
        raise AcceptanceError(
            f"request to {request.selector} failed ({reason})"
        ) from error


def _get_json(base_url: str, path: str) -> dict[str, object]:
    body, _ = _read_response(_request(f"{base_url}{path}"), MAX_JSON_BYTES)
    try:
        value = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"{path} did not return valid JSON") from error
    if not isinstance(value, dict):
        raise AcceptanceError(f"{path} did not return a JSON object")
    return value


def accept_pwa(base_url: str) -> int:
    deadline = time.monotonic() + PWA_DEADLINE_SECONDS
    index, headers = _read_response(
        _request(f"{base_url}/web"), MAX_INDEX_BYTES, deadline=deadline
    )
    try:
        text = index.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AcceptanceError("embedded web index is not UTF-8") from error
    if "Web UI not built" in text or headers.get("Cache-Control") != "no-cache":
        raise AcceptanceError(
            "embedded web index is stubbed or has the wrong cache policy"
        )
    paths = sorted(set(re.findall(r"""(?:src|href)=["']([^"']+)["']""", text)))
    assets = [path for path in paths if HASHED_ASSET.fullmatch(path)]
    if not any(path.endswith(".js") for path in assets):
        raise AcceptanceError(
            "embedded web index has no content-hashed JavaScript asset"
        )
    if len(assets) > MAX_PWA_ASSETS:
        raise AcceptanceError(
            f"embedded web index exceeds the {MAX_PWA_ASSETS}-asset acceptance cap"
        )
    for path in assets:
        body, asset_headers = _read_response(
            _request(f"{base_url}{path}"), MAX_ASSET_BYTES, deadline=deadline
        )
        if (
            not body
            or asset_headers.get("Cache-Control")
            != "public, max-age=31536000, immutable"
        ):
            raise AcceptanceError(
                "embedded hashed asset is empty or lacks immutable caching"
            )
    manifest_paths = sorted(
        set(
            re.findall(
                r"""(?:href|data-manifest-(?:dark|light))=["'](/web/[^"']+[.]webmanifest)["']""",
                text,
            )
        )
    )
    if not manifest_paths:
        raise AcceptanceError("embedded web index has no installable PWA manifest")
    if len(manifest_paths) > MAX_PWA_MANIFESTS:
        raise AcceptanceError(
            f"embedded web index exceeds the {MAX_PWA_MANIFESTS}-manifest acceptance cap"
        )
    fetched_icons: set[str] = set()
    for path in manifest_paths:
        body, manifest_headers = _read_response(
            _request(f"{base_url}{path}"), MAX_JSON_BYTES, deadline=deadline
        )
        try:
            manifest = json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise AcceptanceError("PWA manifest is not valid JSON") from error
        icons = manifest.get("icons") if isinstance(manifest, dict) else None
        required_icons: dict[str, str] = {}
        for icon in icons or []:
            if not isinstance(icon, dict):
                continue
            size = icon.get("sizes")
            source = icon.get("src")
            if size in {"192x192", "512x512"} and isinstance(source, str):
                required_icons[size] = source
        if (
            manifest_headers.get_content_type() != "application/manifest+json"
            or manifest_headers.get("Cache-Control") != "no-cache"
            or manifest.get("id") != "/web"
            or manifest.get("start_url") != "/web"
            or manifest.get("scope") != "/web"
            or manifest.get("display") != "standalone"
            or set(required_icons) != {"192x192", "512x512"}
        ):
            raise AcceptanceError(
                "PWA manifest does not have the installable Codex Voice contract"
            )
        for source in required_icons.values():
            icon_url = urllib.parse.urljoin(f"{base_url}{path}", source)
            parsed = urllib.parse.urlsplit(icon_url)
            if (
                f"{parsed.scheme}://{parsed.netloc}" != base_url
                or not parsed.path.startswith("/web/")
                or parsed.query
                or parsed.fragment
            ):
                raise AcceptanceError(
                    "PWA manifest icon is outside the installed /web application"
                )
            if icon_url in fetched_icons:
                continue
            icon, icon_headers = _read_response(
                _request(icon_url), MAX_PWA_ICON_BYTES, deadline=deadline
            )
            if icon_headers.get_content_type() != "image/png" or not icon.startswith(
                b"\x89PNG\r\n\x1a\n"
            ):
                raise AcceptanceError("PWA manifest icon is missing or not a PNG")
            fetched_icons.add(icon_url)
    return len(assets)


def accept_config(base_url: str) -> tuple[int, str]:
    config = _get_json(base_url, "/web/config")
    default_provider = config.get("defaultProvider")
    providers = config.get("providers")
    if (
        config.get("version") != 2
        or not isinstance(default_provider, str)
        or not default_provider
    ):
        raise AcceptanceError("web config did not prove the expected resolved schema")
    if not isinstance(providers, dict) or not isinstance(
        providers.get(default_provider), dict
    ):
        raise AcceptanceError(
            "web config did not prove the default provider is configured"
        )
    configured = sum(isinstance(value, dict) for value in providers.values())
    return configured, default_provider


def synthesize_canary(base_url: str) -> tuple[bytes, str]:
    body = json.dumps(
        {"model": "codex-voice-canary", "input": CANARY_TEXT, "response_format": "wav"},
        separators=(",", ":"),
    ).encode()
    audio, headers = _read_response(
        _request(
            f"{base_url}/v1/audio/speech",
            data=body,
            headers={"Content-Type": "application/json"},
        ),
        MAX_AUDIO_BYTES,
    )
    content_type = headers.get_content_type()
    if content_type not in {"audio/wav", "audio/wave", "audio/x-wav"}:
        raise AcceptanceError("TTS canary did not return WAV audio")
    if headers.get("X-Codex-Voice-Format") != "wav":
        raise AcceptanceError("TTS canary did not report the requested wav format")
    if len(audio) < 44 or audio[:4] != b"RIFF" or audio[8:12] != b"WAVE":
        raise AcceptanceError("TTS canary returned an invalid WAV envelope")
    return audio, content_type


def _multipart_audio(audio: bytes) -> tuple[bytes, str]:
    boundary = f"codex-voice-{uuid.uuid4().hex}"
    prefix = (
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="response_format"\r\n\r\n'
        "json\r\n"
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="file"; filename="canary.wav"\r\n'
        "Content-Type: audio/wav\r\n\r\n"
    ).encode()
    suffix = f"\r\n--{boundary}--\r\n".encode()
    return prefix + audio + suffix, f"multipart/form-data; boundary={boundary}"


def transcribe_canary(base_url: str, audio: bytes) -> int:
    body, content_type = _multipart_audio(audio)
    response, _ = _read_response(
        _request(
            f"{base_url}/v1/audio/transcriptions",
            data=body,
            headers={"Content-Type": content_type},
        ),
        MAX_JSON_BYTES,
    )
    try:
        value = json.loads(response)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(
            "transcription canary did not return valid JSON"
        ) from error
    transcript = value.get("text") if isinstance(value, dict) else None
    if not isinstance(transcript, str) or not transcript.strip():
        raise AcceptanceError("transcription canary returned no text")
    words = set(re.findall(r"[a-z0-9]+", transcript.lower()))
    if not {"private", "canary", "number"}.issubset(words) or not (
        {"seven", "7"} & words
    ):
        raise AcceptanceError(
            "transcription canary did not match the fixed non-sensitive phrase"
        )
    return len(transcript)


def accept_installed_service(
    base_url: str, source_commit: str, artifact_sha256: str, attestation_path: Path
) -> dict[str, object]:
    if not FULL_SHA.fullmatch(source_commit):
        raise AcceptanceError("source commit must be a lowercase 40-character SHA")
    _require_digest(artifact_sha256, "artifact digest")
    base_url = validate_base_url(base_url)
    attestation = load_attestation(attestation_path, source_commit, artifact_sha256)
    health = _get_json(base_url, "/healthz")
    service_instance_id = health.get("instance_id")
    if (
        not isinstance(service_instance_id, str)
        or not INSTANCE_ID.fullmatch(service_instance_id)
        or service_instance_id != attestation["service_instance_id"]
    ):
        raise AcceptanceError(
            "forwarded service instance does not match the Saga host attestation"
        )
    capabilities_value = health.get("capabilities")
    if health.get("ok") is not True or not isinstance(capabilities_value, dict):
        raise AcceptanceError("healthz did not report a healthy service")
    capabilities = {
        name: capabilities_value.get(name) is True
        for name in ("transcriptions", "speech", "desktop")
    }
    if not all(capabilities.values()):
        raise AcceptanceError(
            "healthz did not report all required installed capabilities"
        )
    asset_count = accept_pwa(base_url)
    provider_count, default_provider = accept_config(base_url)
    audio, audio_content_type = synthesize_canary(base_url)
    transcript_chars = transcribe_canary(base_url, audio)
    return {
        "schema_version": ACCEPTANCE_SCHEMA,
        "service_id": SERVICE_ID,
        "source_commit": source_commit,
        "artifact_sha256": artifact_sha256,
        "installed_binary_sha256": attestation["installed_binary_sha256"],
        "version": attestation["version"],
        "service": {
            "unit": attestation["unit"],
            "unit_user": attestation["unit_user"],
            "listener": attestation["listener"],
            "instance_id": service_instance_id,
            "active": True,
        },
        "health": capabilities,
        "pwa": {"non_stub": True, "hashed_asset_count": asset_count},
        "config": {
            "ready": True,
            "sha256": attestation["config_sha256"],
            "default_provider": default_provider,
            "configured_provider_count": provider_count,
            "provider_environment_ready": True,
        },
        "codex_auth_ready": True,
        "tts": {"content_type": audio_content_type, "bytes": len(audio), "wav": True},
        "transcription": {"characters": transcript_chars, "fixed_phrase_matched": True},
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base-url",
        required=True,
        help="loopback URL reached locally or through SSH forwarding",
    )
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--artifact-sha256", required=True)
    parser.add_argument("--installed-attestation", required=True, type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        result = accept_installed_service(
            args.base_url,
            args.source_commit,
            args.artifact_sha256,
            args.installed_attestation,
        )
    except (AcceptanceError, OSError) as error:
        print(f"installed acceptance failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
