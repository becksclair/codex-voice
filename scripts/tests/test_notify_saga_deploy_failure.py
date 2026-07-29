from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "notify_saga_deploy_failure", ROOT / "scripts/notify_saga_deploy_failure.py"
)
assert SPEC and SPEC.loader
MOD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MOD)


class NotifySagaDeployFailureTests(unittest.TestCase):
    def test_message_contains_only_bounded_failure_contract(self) -> None:
        message = MOD.build_message(
            sender="automation@example.invalid",
            recipient="operator@example.invalid",
            source_commit="a" * 40,
            archive_sha256="b" * 64,
            failed_stage="installed-acceptance",
            diagnostic_pointer="https://git.example.invalid/bex/codex-voice/actions/runs/123",
        )
        body = message.get_content()
        self.assertIn("a" * 40, body)
        self.assertIn("b" * 64, body)
        self.assertIn("installed-acceptance", body)
        self.assertIn("Corrective next action:", body)
        for forbidden in (
            "config.json",
            "SMTP_PASSWORD",
            "transcript text",
            "provider.env",
        ):
            self.assertNotIn(forbidden, body)

    def test_missing_or_unknown_stage_is_workflow_setup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stage"
            self.assertEqual(MOD.read_stage(path), "workflow-setup")
            path.write_text("tray-restart\n", encoding="utf-8")
            self.assertEqual(MOD.read_stage(path), "workflow-setup")

    def test_pointer_rejects_credentials_and_non_https(self) -> None:
        for value in (
            "http://git.example.invalid/actions/runs/1",
            "https://token@git.example.invalid/actions/runs/1",
        ):
            with self.subTest(value=value), self.assertRaises(MOD.NotificationError):
                MOD.validate_diagnostic_pointer(value)

    def test_empty_archive_digest_is_explicitly_unavailable(self) -> None:
        message = MOD.build_message(
            sender="automation@example.invalid",
            recipient="operator@example.invalid",
            source_commit="a" * 40,
            archive_sha256="",
            failed_stage="workflow-setup",
            diagnostic_pointer="https://git.example.invalid/actions/runs/123",
        )
        self.assertIn("unavailable before immutable read-back", message.get_content())


if __name__ == "__main__":
    unittest.main()
