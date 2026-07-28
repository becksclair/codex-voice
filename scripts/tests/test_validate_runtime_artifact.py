from __future__ import annotations

import gzip
import hashlib
import io
import os
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from validate_runtime_artifact import (  # noqa: E402
    ValidationError,
    canonical_manifest_bytes,
    smoke_environment,
    validate_artifact,
)


SHA = "0123456789abcdef0123456789abcdef01234567"


class ArtifactFixture:
    def __init__(self, root: Path) -> None:
        self.archive = root / f"codex-voice-linux-amd64-{SHA}.tar.gz"

    def write(
        self,
        *,
        manifest: bytes | None = None,
        binary_mode: int = 0o755,
        binary_machine: int = 62,
        extra_member: str | None = None,
        binary_kind: bytes = tarfile.REGTYPE,
        gzip_mtime: int = 0,
    ) -> Path:
        tar_bytes = io.BytesIO()
        with tarfile.open(fileobj=tar_bytes, mode="w", format=tarfile.GNU_FORMAT) as bundle:
            self._add(bundle, "codex-voice", b"", tarfile.DIRTYPE, 0o755)
            self._add(
                bundle,
                "codex-voice/artifact.json",
                manifest if manifest is not None else canonical_manifest_bytes(SHA),
                tarfile.REGTYPE,
                0o644,
            )
            elf = bytearray(20)
            elf[:6] = b"\x7fELF\x02\x01"
            elf[18:20] = binary_machine.to_bytes(2, "little")
            self._add(bundle, "codex-voice/codex-voice", bytes(elf), binary_kind, binary_mode)
            if extra_member:
                self._add(bundle, extra_member, b"unexpected", tarfile.REGTYPE, 0o644)
        with self.archive.open("wb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=gzip_mtime) as compressed:
                compressed.write(tar_bytes.getvalue())
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.archive.with_name(f"{self.archive.name}.sha256").write_text(
            f"{digest}  {self.archive.name}\n"
        )
        return self.archive

    @staticmethod
    def _add(bundle: tarfile.TarFile, name: str, body: bytes, kind: bytes, mode: int) -> None:
        member = tarfile.TarInfo(name)
        member.type = kind
        member.mode = mode
        member.uid = member.gid = member.mtime = 0
        member.size = len(body) if kind == tarfile.REGTYPE else 0
        bundle.addfile(member, io.BytesIO(body) if member.size else None)


class ValidateRuntimeArtifactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.fixture = ArtifactFixture(Path(self.temp.name))

    def assert_rejected(self, **changes: object) -> None:
        archive = self.fixture.write(**changes)
        with self.assertRaises(ValidationError):
            validate_artifact(archive, SHA)

    def test_accepts_exact_contract(self) -> None:
        validate_artifact(self.fixture.write(), SHA)

    def test_rejects_noncanonical_manifest(self) -> None:
        self.assert_rejected(manifest=b'{}\n')

    def test_rejects_unexpected_member(self) -> None:
        self.assert_rejected(extra_member="codex-voice/README")

    def test_rejects_non_executable_binary(self) -> None:
        self.assert_rejected(binary_mode=0o644)

    def test_rejects_symlink_binary(self) -> None:
        self.assert_rejected(binary_kind=tarfile.SYMTYPE)

    def test_rejects_wrong_elf_machine(self) -> None:
        self.assert_rejected(binary_machine=183)

    def test_rejects_nonzero_gzip_mtime(self) -> None:
        self.assert_rejected(gzip_mtime=1)

    def test_rejects_incorrect_checksum_sidecar(self) -> None:
        archive = self.fixture.write()
        archive.with_name(f"{archive.name}.sha256").write_text(f"{'0' * 64}  {archive.name}\n")
        with self.assertRaises(ValidationError):
            validate_artifact(archive, SHA)

    def test_rejects_short_or_uppercase_commit(self) -> None:
        archive = self.fixture.write()
        for invalid in (SHA[:12], SHA.upper()):
            with self.subTest(invalid=invalid), self.assertRaises(ValidationError):
                validate_artifact(archive, invalid)

    def test_smoke_environment_excludes_live_credentials_and_redirects_state(self) -> None:
        runtime = Path(self.temp.name) / "runtime"
        inherited = {
            "PATH": "/usr/bin",
            "CODEX_VOICE_TRANSCRIBER_TOKEN": "live-token",
            "GEMINI_API_KEY": "live-provider-key",
            "XDG_STATE_HOME": "/live/state",
        }
        with patch.dict(os.environ, inherited, clear=True):
            environment = smoke_environment(runtime)
        self.assertEqual(environment["PATH"], "/usr/bin")
        self.assertEqual(environment["CODEX_VOICE_TRANSCRIBER_TOKEN"], "artifact-smoke-not-secret")
        self.assertNotIn("GEMINI_API_KEY", environment)
        for key in (
            "HOME",
            "CODEX_HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
        ):
            self.assertTrue(Path(environment[key]).is_relative_to(runtime), key)


if __name__ == "__main__":
    unittest.main()
