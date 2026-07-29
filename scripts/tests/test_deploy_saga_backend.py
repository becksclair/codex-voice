from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import stat
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "deploy_saga_backend", ROOT / "scripts/deploy_saga_backend.py"
)
assert SPEC and SPEC.loader
MOD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MOD)


class DeploySagaBackendTests(unittest.TestCase):
    def write_protected(self, path: Path, text: str) -> None:
        path.write_text(text, encoding="utf-8")
        path.chmod(0o600)

    def test_protected_inputs_require_exact_provider_names(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "config.json"
            provider = root / "provider.env"
            self.write_protected(
                config,
                json.dumps(
                    {
                        "version": 1,
                        "providers": {
                            "google": {"models": ["gemini-2.5-pro-preview-tts"]},
                            "elevenlabs": {
                                "models": ["eleven_v3"],
                                "streamGain": 1.0,
                                "textNormalization": "auto",
                            },
                        },
                    }
                ),
            )
            self.write_protected(
                provider,
                "GEMINI_API_KEY=secret-one\nELEVENLABS_API_KEY=secret-two\n",
            )

            MOD.validate_protected_inputs(config, provider)

            self.write_protected(
                provider,
                "GEMINI_API_KEY=secret-one\nGOOGLE_API_KEY=duplicate-choice\n"
                "ELEVENLABS_API_KEY=secret-two\n",
            )
            with self.assertRaisesRegex(MOD.DeployError, "exactly one key name"):
                MOD.validate_protected_inputs(config, provider)

            self.write_protected(
                provider,
                "GEMINI_API_KEY=secret-one\nELEVENLABS_API_KEY=secret-two\n"
                "EXTRA_KEY=secret-extra\n",
            )
            with self.assertRaisesRegex(MOD.DeployError, "do not exactly match"):
                MOD.validate_protected_inputs(config, provider)

    def test_protected_inputs_honor_custom_provider_environment_name(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "config.json"
            provider = root / "provider.env"
            self.write_protected(
                config,
                json.dumps(
                    {
                        "version": 1,
                        "providers": {"google": {"models": ["model"]}},
                        "advanced": {
                            "providers": {"google": {"apiKeyEnv": "customGoogleKey"}}
                        },
                    }
                ),
            )
            self.write_protected(provider, "customGoogleKey=secret\n")
            MOD.validate_protected_inputs(config, provider)

    def test_protected_inputs_reject_loose_mode_and_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.write_text("value", encoding="utf-8")
            source.chmod(0o640)
            with self.assertRaisesRegex(MOD.DeployError, "mode 0600"):
                MOD.require_protected_regular_file(source, "source")

            source.chmod(0o600)
            link = root / "link"
            link.symlink_to(source)
            with self.assertRaisesRegex(MOD.DeployError, "non-symlink"):
                MOD.require_protected_regular_file(link, "source")

    def test_sidecar_is_bound_to_exact_archive_name(self) -> None:
        name = "codex-voice-linux-amd64-" + "a" * 40 + ".tar.gz"
        digest = "b" * 64
        self.assertEqual(MOD.parse_sidecar(f"{digest}  {name}\n", name), digest)
        with self.assertRaisesRegex(MOD.DeployError, "invalid identity"):
            MOD.parse_sidecar(f"{digest}  other.tar.gz\n", name)

    def test_server_url_rejects_credentials_and_non_origin_paths(self) -> None:
        self.assertEqual(
            MOD.validate_server_url("https://git.example.invalid/"),
            "https://git.example.invalid",
        )
        for value in (
            "http://git.example.invalid",
            "https://token@git.example.invalid",
            "https://git.example.invalid/path",
        ):
            with self.subTest(value=value), self.assertRaises(MOD.DeployError):
                MOD.validate_server_url(value)

    def test_stage_file_is_restrictive_and_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stage"
            MOD.write_stage(path, "download")
            self.assertEqual(path.read_text(encoding="utf-8"), "download\n")
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            with self.assertRaisesRegex(MOD.DeployError, "invalid deployment stage"):
                MOD.write_stage(path, "tray-restart")

    def test_deployment_vars_target_backend_only(self) -> None:
        values = MOD.deployment_vars(
            source_commit="a" * 40,
            archive_url=(
                "https://git.example.invalid/api/packages/bex/generic/codex-voice/"
                + "a" * 40
                + "/codex-voice-linux-amd64-"
                + "a" * 40
                + ".tar.gz"
            ),
            archive_sha256="b" * 64,
            saga_source=Path("/tmp/saga-source"),
            config_source=Path("/protected/config.json"),
            provider_env_source=Path("/protected/provider.env"),
        )
        encoded = json.dumps(values, sort_keys=True)
        self.assertEqual(values["service_id"], "codex-voice")
        self.assertNotIn("codex-voice-server.service", encoded)
        self.assertNotIn("codex-voice.service", encoded)
        self.assertNotIn("tray", encoded)
        self.assertNotIn("secret", encoded)

    def test_workflow_is_serialized_backend_only_and_failure_only_notified(
        self,
    ) -> None:
        workflow = (ROOT / ".gitea/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertIn("runs-on: asgard-build-1", workflow)
        self.assertIn("python3 scripts/deploy_saga_backend.py", workflow)
        self.assertIn("if: failure()", workflow)
        self.assertIn("python3 scripts/notify_saga_deploy_failure.py", workflow)
        deploy_job = workflow.split("  deploy-saga-backend:", 1)[1]
        self.assertIn("GITEA_REPOSITORY: ${{ github.repository }}", deploy_job)
        self.assertIn("GITEA_RUN_ID: ${{ github.run_id }}", deploy_job)
        self.assertNotIn("systemctl restart codex-voice-server.service", workflow)
        self.assertNotIn("systemctl restart codex-voice.service", workflow)
        self.assertNotIn("codex-voice-wrapper run", workflow)


if __name__ == "__main__":
    unittest.main()
