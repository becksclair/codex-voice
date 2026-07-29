#!/usr/bin/env python3
"""Send one redacted Codex Voice deployment-failure email.

The workflow invokes this only after its backend deployment step reaches a
terminal failure. Recipient, sender, transport, and credentials are external
configuration; none are printed.
"""

from __future__ import annotations

import argparse
from email.message import EmailMessage
import os
from pathlib import Path
import re
import smtplib
import ssl
import urllib.parse


COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
KNOWN_STAGES = {
    "preflight",
    "download",
    "artifact-validation",
    "selected-service-install",
    "backend-activation",
    "installed-acceptance",
    "workflow-setup",
}
NEXT_ACTION = {
    "preflight": "Provision or correct the protected deployment inputs, then rerun the failed main workflow.",
    "download": "Verify the immutable Gitea package files are readable, then rerun the failed main workflow.",
    "artifact-validation": "Inspect the producer artifact validation failure and publish a corrected main commit.",
    "selected-service-install": "Inspect the redacted Ansible run diagnostics and correct the selected-service contract.",
    "backend-activation": "Inspect the Saga backend unit status and correct the unit or runtime prerequisite.",
    "installed-acceptance": "Inspect the bounded installed-acceptance result and correct the failing backend behavior.",
    "workflow-setup": "Inspect the workflow setup failure and correct the runner or external notification configuration.",
}


class NotificationError(RuntimeError):
    """A bounded notification configuration failure."""


def required_environment(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise NotificationError(f"required notification setting is absent: {name}")
    if "\r" in value or "\n" in value:
        raise NotificationError(f"notification setting has an invalid shape: {name}")
    return value


def read_stage(path: Path) -> str:
    try:
        stage = path.read_text(encoding="utf-8").strip()
    except OSError:
        return "workflow-setup"
    return stage if stage in KNOWN_STAGES else "workflow-setup"


def validate_diagnostic_pointer(value: str) -> str:
    parsed = urllib.parse.urlparse(value)
    if (
        parsed.scheme != "https"
        or not parsed.netloc
        or parsed.username
        or parsed.password
        or parsed.params
        or parsed.fragment
        or len(value) > 500
    ):
        raise NotificationError("diagnostic pointer must be one bounded HTTPS URL")
    return value


def build_message(
    *,
    sender: str,
    recipient: str,
    source_commit: str,
    archive_sha256: str,
    failed_stage: str,
    diagnostic_pointer: str,
) -> EmailMessage:
    if not COMMIT_RE.fullmatch(source_commit):
        raise NotificationError("source commit has an invalid shape")
    if archive_sha256 and not SHA256_RE.fullmatch(archive_sha256):
        raise NotificationError("archive SHA-256 has an invalid shape")
    if failed_stage not in KNOWN_STAGES:
        raise NotificationError("failed stage is not bounded")
    pointer = validate_diagnostic_pointer(diagnostic_pointer)
    digest = archive_sha256 or "unavailable before immutable read-back"

    message = EmailMessage()
    message["Subject"] = f"Codex Voice Saga deployment failed at {failed_stage}"
    message["From"] = sender
    message["To"] = recipient
    message.set_content(
        "\n".join(
            [
                "Codex Voice automatic Saga backend deployment failed.",
                "",
                f"Source commit: {source_commit}",
                f"Artifact SHA-256: {digest}",
                f"Failed stage: {failed_stage}",
                f"Diagnostic pointer: {pointer}",
                f"Corrective next action: {NEXT_ACTION[failed_stage]}",
                "",
                "This notification intentionally excludes configuration, tokens, provider values, auth material, transcripts, and audio.",
            ]
        )
        + "\n"
    )
    return message


def send_message(message: EmailMessage) -> None:
    host = required_environment("CODEX_VOICE_FAILURE_SMTP_HOST")
    port_text = required_environment("CODEX_VOICE_FAILURE_SMTP_PORT")
    username = required_environment("CODEX_VOICE_FAILURE_SMTP_USERNAME")
    password = required_environment("CODEX_VOICE_FAILURE_SMTP_PASSWORD")
    security = os.environ.get("CODEX_VOICE_FAILURE_SMTP_SECURITY", "tls").strip()
    try:
        port = int(port_text)
    except ValueError as error:
        raise NotificationError("SMTP port has an invalid shape") from error
    if not 1 <= port <= 65535 or security not in {"tls", "starttls"}:
        raise NotificationError("SMTP transport configuration is invalid")

    context = ssl.create_default_context()
    if security == "tls":
        with smtplib.SMTP_SSL(host, port, timeout=30, context=context) as client:
            client.login(username, password)
            client.send_message(message)
    else:
        with smtplib.SMTP(host, port, timeout=30) as client:
            client.ehlo()
            client.starttls(context=context)
            client.ehlo()
            client.login(username, password)
            client.send_message(message)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--archive-sha256", default="")
    parser.add_argument("--stage-file", required=True, type=Path)
    parser.add_argument("--diagnostic-pointer", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        message = build_message(
            sender=required_environment("CODEX_VOICE_FAILURE_EMAIL_FROM"),
            recipient=required_environment("CODEX_VOICE_FAILURE_EMAIL_TO"),
            source_commit=args.source_commit,
            archive_sha256=args.archive_sha256,
            failed_stage=read_stage(args.stage_file),
            diagnostic_pointer=args.diagnostic_pointer,
        )
        send_message(message)
    except (NotificationError, OSError, smtplib.SMTPException):
        print(
            "Codex Voice deployment failure email could not be sent; original deployment failure remains authoritative.",
            file=os.sys.stderr,
        )
        return 1
    print("Codex Voice deployment failure email sent")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
