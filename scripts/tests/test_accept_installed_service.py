from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from accept_installed_service import (  # noqa: E402
    ATTESTATION_SCHEMA,
    AcceptanceError,
    accept_installed_service,
    load_attestation,
    validate_base_url,
)


SOURCE_COMMIT = "0123456789abcdef0123456789abcdef01234567"
ARTIFACT_SHA256 = "1" * 64
BINARY_SHA256 = hashlib.sha256(b"installed-binary").hexdigest()
CONFIG_SHA256 = hashlib.sha256(b"protected-config").hexdigest()
INSTANCE_ID = "abcdef0123456789abcdef0123456789"
WAV = b"RIFF" + (b"\x00" * 4) + b"WAVE" + (b"\x00" * 64)
PNG = b"\x89PNG\r\n\x1a\n" + (b"\x00" * 16)


def attestation(**changes: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": ATTESTATION_SCHEMA,
        "service_id": "codex-voice",
        "source_commit": SOURCE_COMMIT,
        "artifact_sha256": ARTIFACT_SHA256,
        "artifact_binary_sha256": BINARY_SHA256,
        "installed_binary_sha256": BINARY_SHA256,
        "version": "codex-voice 0.1.0",
        "unit": "codex-voice.service",
        "unit_user": "ubuntu",
        "active_state": "active",
        "sub_state": "running",
        "listener": "127.0.0.1:3845",
        "service_instance_id": INSTANCE_ID,
        "config_sha256": CONFIG_SHA256,
        "config_ready": True,
        "provider_environment_ready": True,
        "codex_auth_ready": True,
    }
    value.update(changes)
    return value


class FakeService(BaseHTTPRequestHandler):
    transcript = "The private canary number is seven."
    index = b'<html><link rel="manifest" href="/web/manifest.webmanifest"><script src="/web/assets/app-12345678.js"></script></html>'
    config: object = {
        "version": 2,
        "defaultProvider": "google",
        "providers": {"google": {"voice": "fixture"}},
        "secretFixture": "must-never-appear",
    }
    icon_sources = True

    def log_message(self, _format: str, *_args: object) -> None:
        pass

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/healthz":
            self._json(
                {
                    "ok": True,
                    "instance_id": INSTANCE_ID,
                    "capabilities": {
                        "transcriptions": True,
                        "speech": True,
                        "desktop": True,
                    },
                }
            )
        elif self.path == "/web":
            self._send(
                self.index,
                "text/html",
                "no-cache",
            )
        elif self.path == "/web/assets/app-12345678.js":
            self._send(
                b"console.log('fixture')",
                "text/javascript",
                "public, max-age=31536000, immutable",
            )
        elif self.path == "/web/config":
            self._json(self.config)
        elif self.path == "/web/manifest.webmanifest":
            icons = (
                [
                    {"src": "icon-192.png", "sizes": "192x192"},
                    {"src": "icon-512.png", "sizes": "512x512"},
                ]
                if self.icon_sources
                else [{"sizes": "192x192"}, {"sizes": "512x512"}]
            )
            self._send(
                json.dumps(
                    {
                        "id": "/web",
                        "start_url": "/web",
                        "scope": "/web",
                        "display": "standalone",
                        "icons": icons,
                    }
                ).encode(),
                "application/manifest+json",
                "no-cache",
            )
        elif self.path in {"/web/icon-192.png", "/web/icon-512.png"}:
            self._send(PNG, "image/png", "no-cache")
        else:
            self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        if self.path == "/v1/audio/speech":
            request = json.loads(body)
            if request["input"] != "The private canary number is seven.":
                self.send_error(400)
                return
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("X-Codex-Voice-Format", "wav")
            self.send_header("Content-Length", str(len(WAV)))
            self.end_headers()
            self.wfile.write(WAV)
        elif self.path == "/v1/audio/transcriptions":
            if WAV not in body:
                self.send_error(400)
                return
            self._json({"text": self.transcript})
        else:
            self.send_error(404)

    def _json(self, value: object) -> None:
        self._send(json.dumps(value).encode(), "application/json", "no-store")

    def _send(self, body: bytes, content_type: str, cache_control: str) -> None:
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Cache-Control", cache_control)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class InstalledAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.attestation_path = Path(self.temp.name) / "attestation.json"
        self.attestation_path.write_text(json.dumps(attestation()))
        FakeService.transcript = "The private canary number is seven."
        FakeService.index = b'<html><link rel="manifest" href="/web/manifest.webmanifest"><script src="/web/assets/app-12345678.js"></script></html>'
        FakeService.config = {
            "version": 2,
            "defaultProvider": "google",
            "providers": {"google": {"voice": "fixture"}},
            "secretFixture": "must-never-appear",
        }
        FakeService.icon_sources = True
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), FakeService)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def test_accepts_installed_service_without_echoing_config(self) -> None:
        result = accept_installed_service(
            self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
        )
        serialized = json.dumps(result)
        self.assertTrue(result["transcription"]["fixed_phrase_matched"])
        self.assertTrue(result["tts"]["wav"])
        self.assertNotIn("must-never-appear", serialized)

    def test_requires_loopback_or_forward_url(self) -> None:
        for invalid in (
            "https://127.0.0.1:3845",
            "http://100.64.0.1:3845",
            "http://localhost",
        ):
            with self.subTest(invalid=invalid), self.assertRaises(AcceptanceError):
                validate_base_url(invalid)

    def test_rejects_installed_binary_mismatch(self) -> None:
        self.attestation_path.write_text(
            json.dumps(attestation(installed_binary_sha256="2" * 64))
        )
        with self.assertRaisesRegex(AcceptanceError, "installed binary digest"):
            load_attestation(self.attestation_path, SOURCE_COMMIT, ARTIFACT_SHA256)

    def test_rejects_forwarded_service_instance_mismatch(self) -> None:
        self.attestation_path.write_text(
            json.dumps(attestation(service_instance_id="0" * 32))
        )
        with self.assertRaisesRegex(AcceptanceError, "service instance"):
            accept_installed_service(
                self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
            )

    def test_rejects_unproven_host_readiness(self) -> None:
        self.attestation_path.write_text(
            json.dumps(attestation(codex_auth_ready=False))
        )
        with self.assertRaisesRegex(AcceptanceError, "codex_auth_ready"):
            load_attestation(self.attestation_path, SOURCE_COMMIT, ARTIFACT_SHA256)

    def test_rejects_transcription_that_does_not_match_canary(self) -> None:
        FakeService.transcript = "unrelated words"
        with self.assertRaisesRegex(AcceptanceError, "fixed non-sensitive phrase"):
            accept_installed_service(
                self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
            )

    def test_rejects_config_without_default_provider(self) -> None:
        FakeService.config = {
            "version": 2,
            "defaultProvider": "google",
            "providers": {},
        }
        with self.assertRaisesRegex(AcceptanceError, "default provider"):
            accept_installed_service(
                self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
            )

    def test_rejects_web_shell_without_installable_manifest(self) -> None:
        FakeService.index = (
            b'<html><script src="/web/assets/app-12345678.js"></script></html>'
        )
        with self.assertRaisesRegex(AcceptanceError, "PWA manifest"):
            accept_installed_service(
                self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
            )

    def test_rejects_unbounded_asset_fanout(self) -> None:
        assets = "".join(
            f'<script src="/web/assets/app-{index:08d}.js"></script>'
            for index in range(65)
        )
        FakeService.index = (
            f'<html><link rel="manifest" href="/web/manifest.webmanifest">{assets}</html>'
        ).encode()
        with self.assertRaisesRegex(AcceptanceError, "asset acceptance cap"):
            accept_installed_service(
                self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
            )

    def test_rejects_manifest_icons_without_sources(self) -> None:
        FakeService.icon_sources = False
        with self.assertRaisesRegex(
            AcceptanceError, "installable Codex Voice contract"
        ):
            accept_installed_service(
                self.base_url, SOURCE_COMMIT, ARTIFACT_SHA256, self.attestation_path
            )


if __name__ == "__main__":
    unittest.main()
