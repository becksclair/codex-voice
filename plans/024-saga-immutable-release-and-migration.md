# Saga immutable release and migration

> Historical implementation plan. The bespoke selected-service installer,
> installed-service attestation harness, SMTP failure notifier, and direct
> Asgard deployment path described below were retired after Codex Voice moved to
> the manifest-registered `saga-deploy` controller. Keep this document as
> migration evidence, not current operational guidance.

Status: **PHASE 1 PROVEN; PHASE 2 IMPLEMENTATION UNCOMMITTED — the immutable
artifact has been published/read back, installed as the Saga backend, and passed
the private installed-service acceptance. Automatic delivery is being prepared
but is not published or active. Public routing and the Asgard tray are
unchanged.**

This plan separates confirmed current state from the approved target. It is the
release/cutover contract for moving Codex Voice from Asgard to Saga.

## Confirmed current state

- A release must build `web/dist` before Rust: cargo otherwise embeds a stub PWA.
- The application server is no-auth by default. It allows loopback and Tailnet
  binds; the migrated Saga service will use loopback only.
- Codex Voice is two services, not one host workload. On Asgard,
  `codex-voice-server.service` runs `/usr/local/bin/codex-voice-wrapper server
  --bind 100.120.202.119:3845` as `bex`; this is the backend being migrated.
  Separately, `codex-voice.service` runs `/usr/local/bin/codex-voice-wrapper run`
  as `bex`, owns the graphical tray/Tauri process, and self-hosts its desktop
  origin on `127.0.0.1:3846`. There is no `codex-voice-tray.service` unit.
- The unit name is intentionally host-scoped: Saga's backend is
  `/etc/systemd/system/codex-voice.service`, running
  `/opt/saga-services/codex-voice/codex-voice server --bind 127.0.0.1:3845`
  as `ubuntu:ubuntu`. Automatic deployment may manage this Saga unit only. It
  must never copy, stop, restart, enable, disable, or otherwise manage Asgard's
  `codex-voice.service` tray unit.
- The Asgard tray currently has no unit-level endpoint override. Backend client
  resolution checks `CODEX_VOICE_TRANSCRIBER_URL` plus
  `CODEX_VOICE_TRANSCRIBER_TOKEN` before the private discovery file at
  `${XDG_STATE_HOME:-~/.local/state}/codex-voice/transcriber.json`. The running
  Asgard backend owns that discovery file and its process-bound token. Therefore
  the later cutover is a tray-client endpoint/config switch after Saga backend
  acceptance, not a tray process or host migration.
- The active Asgard config is the service user's resolved
  `/home/bex/.config/codex-voice/config.json`. The proven Saga backend config is
  `/home/ubuntu/.config/codex-voice/config.json`; provider values remain in the
  separately owned `/srv/services-state/codex-voice/codex-voice.env`, and Saga's
  existing auth remains `/home/ubuntu/.codex/auth.json` pointing to its
  host-owned auth file. None belongs in an artifact or workflow output.
- Saga currently terminates Tailnet-only `voice.heliasar.com` ingress and proxies
  to the Asgard backend at `100.120.202.119:3845`. The compatibility host
  `codex-voice.heliasar.com` redirects to `voice.heliasar.com`. Asgard remains
  public backend owner until a separately authorized cutover.
- Saga uses a central Ansible, fixed-root, selected-service deployment lane.
- The active Gitea producer runner label is `saga-build`. The established
  host-native deployment runner is `asgard-build-1`; it already owns the proven
  operator SSH route to `saga`. Its capacity is one, and the Codex Voice workflow
  adds a service-specific non-cancelling concurrency group as the explicit
  serialization boundary.
- The Codex Voice repository now owns a least-privilege `PACKAGE_TOKEN` Actions
  secret for immutable Gitea package publication/read-back; its value remains
  outside the repository and workflow output.
- Saga has normal Codex auth already. This migration does not copy auth or log in.
- Phase 1 used central homelab commit
  `674eb3d15c496dc1ce6d6b4408bfe33497092226` and Saga consumer commit
  `1b4c308e4c82570b4715c4e4951e899f58c7d098`. Phase 2 pins those proven
  contracts instead of consuming mutable local branches.
- No configured Asgard/Saga mail transport or existing Codex Voice mail command
  is present. The workflow therefore owns a standard SMTP adapter, while the
  recipient, sender, SMTP endpoint, and credentials remain external Gitea
  Actions secrets and are never repository literals or diagnostic output.

## Approved artifact and automation contract

Every successful `main` push runs the bounded producer workflow:

1. Build the Vite app and locked release binary, validate it, and publish an
   immutable artifact under the full source commit SHA.

After the one-time private Saga canary proves the consumer lane, every
successful producer publication automatically triggers the separate consumer
workflow:

2. Consume that exact published artifact and deploy only the
   `codex-voice` selected service on Saga. Deployment never rebuilds source or
   resolves a mutable `latest` artifact.

Producer publication is required to create the Phase 1 artifact. Only the
automatic deploy trigger waits for the one-time private Saga canary. Canonical
cutover is later and separately authorized; it is not part of the
automatic-delivery completion claim.

Artifact contract:

- `codex-voice-linux-amd64-<40-char-lowercase-commit>.tar.gz` and adjacent
  `.sha256` sidecar;
- exactly `codex-voice/`, `codex-voice/artifact.json`, and the mode-0755
  `codex-voice/codex-voice` ELF64 x86-64 executable;
- manifest schema `saga-binary-service-artifact/v1`, with only
  `schema_version`, `service_id`, `source_commit`, `platform`, `executable`, and
  `health_path`;
- sorted members, zero timestamps, numeric root ownership, canonical compact
  JSON, and gzip without timestamp/name metadata;
- producer proof: strict archive/checksum/manifest/ELF validation, extracted
  `--version`, loopback `/healthz` with `capabilities.desktop=true`, non-stub
  `/web`, and successful content-hashed immutable asset fetches.

Local producer entrypoint:

```bash
scripts/package_linux_amd64.sh <full-commit-sha> [output-directory]
```

It refuses dirty source, a SHA other than `HEAD`, a non-Linux-x86-64 builder, or
existing output. It does not publish or deploy.

## Protected service state

Before canary, inspect the effective Asgard service user, `HOME`,
`XDG_CONFIG_HOME`, wrapper, and application resolver to identify the one active
Codex Voice config file. Resolve the fixed Saga destination independently from
the final unit. Do not guess capitalization/path, copy a directory, or include
`~/.codex/auth.json`.

Transfer exactly that config as a protected service-scoped input. Validate its
schema and required keys without values; install before startup with final owner
and restrictive mode; record only a checksum/fingerprint. Exclude it from
artifacts, repository, logs, and workflow output. Provider keys are separate
protected environment inputs.

Under the final Saga unit identity/sandbox, check that Saga's existing Codex
auth is present and usable. Report only success or bounded redacted errors; do
not transfer, rewrite, or print it.

## Migration and cutover acceptance

1. **Private Saga canary, before any ingress change.** Install the immutable
   artifact and protected config, run the no-auth server at
   `127.0.0.1:3845`, and validate existing Codex auth. From a second Tailnet
   client, use an SSH local forward to that loopback listener. Accept health,
   embedded PWA/assets, config/provider readiness, a small non-sensitive
   transcription, and consumer TTS. Direct Saga Tailnet-IP access to 3845 must
   fail.
2. **Automatic delivery proof.** Enable the successful-main producer and its
   serialized `codex-voice` consumer. Prove a real push publishes, reads back,
   installs, activates, and accepts the exact artifact through the same
   installed-service contract. Terminal deploy/consumer failures email Bex;
   success sends no mail.
3. **Canonical Tailnet-only cutover, separately authorized follow-up.** DNS
   already resolves `voice.heliasar.com` to Saga. In central service intent,
   change only the Codex Voice runtime/site ownership from Asgard at
   `100.120.202.119:3845` to Saga at `127.0.0.1:3845`, regenerate Saga Caddy,
   and prove that the generated final `handle` now uses
   `reverse_proxy 127.0.0.1:3845`. Preserve the existing direct
   `POST /_codex/responses` ChatGPT handler, the rejecting `/_codex/*` handler,
   TLS, headers, encoding, and Tailnet-only DNS/ingress ownership. From a
   Tailnet consumer, accept canonical DNS/TLS, the same health instance and
   capabilities, PWA/assets, config surface, one bounded transcription, and
   one TTS synthesis.
4. **Tray endpoint switch, separately authorized follow-up.** Keep the graphical
   tray on Asgard. Replace its process-local backend discovery dependency with
   the accepted canonical Saga backend by setting both
   `CODEX_VOICE_TRANSCRIBER_URL` and `CODEX_VOICE_TRANSCRIBER_TOKEN` in the
   Asgard tray unit's protected environment seam. The token remains a bounded
   request identifier, not application authentication; Saga stays no-auth and
   Tailnet-only. Prove tray transcription against the accepted Saga instance.
   This is a client endpoint/config update, not a binary copy or process move.
5. **Backend ownership closeout.** Only after canonical and tray-client live
   acceptance, stop/disable Asgard `codex-voice-server.service` and remove its
   backend/public-route ownership references. Leave Asgard
   `codex-voice.service` enabled and running as the tray.

Rollback acceptance, drills, and proof are not gates. Ordinary failure handling
stops the failed stage, reports it, and leaves corrective action to the operator.

## Terminal deployment-failure email

Every automatic deployment reaching a terminal failed deploy or consumer-check
state sends exactly one failure-only email through Bex's existing operational
notification configuration. Success sends no email. Include only source commit,
archive SHA-256, failed stage, a bounded redacted run/diagnostic pointer, and the
next corrective action. Never include config, auth, provider keys, tokens, or
environment dumps. A mail-send failure is a separate bounded error and must not
mask the original deployment failure.

## Deployment inputs and secret ownership (names/categories only)

- **Protected file input:** the one resolved active Codex Voice service config;
  its source path, destination path, owner/group, and mode remain evidence to
  resolve. It is not a Gitea artifact or workflow-output value.
- **Existing host auth:** Saga's normal `~/.codex/auth.json`, resolved under the
  final service user. It is readiness evidence only, not transferred material or
  a workflow secret.
- **Provider environment:** only the environment-variable names declared by the
  active config's provider `apiKeyEnv` fields. Resolve names without values; do
  not assume defaults when the migrated config names custom variables.
- **Artifact read credential:** none. The exact Generic Package files are
  anonymously readable; only the producer publication step receives
  `PACKAGE_TOKEN`.
- **Saga deploy credential:** none added to this repository. The
  `asgard-build-1` host runner reuses its established operator SSH identity for
  `saga` and the locked central Ansible executable.
- **Protected controller source:** the exact active config remains
  `/home/bex/.config/codex-voice/config.json`. Before workflow activation,
  provision the separately owned provider-only environment at
  `/home/bex/.config/codex-voice/saga-provider.env`, owned by `bex`, mode 0600,
  containing exactly one effective key name for each enabled provider. An
  explicit advanced `apiKeyEnv` wins; otherwise use exactly one supported
  application default (`GEMINI_API_KEY` or `GOOGLE_API_KEY`, and
  `ELEVENLABS_API_KEY` or `ELEVEN_API_KEY`). The workflow checks names, type,
  ownership, and mode without printing values.
- **Failure email:** configure Gitea Actions secrets
  `CODEX_VOICE_FAILURE_EMAIL_TO`, `CODEX_VOICE_FAILURE_EMAIL_FROM`,
  `CODEX_VOICE_FAILURE_SMTP_HOST`, `CODEX_VOICE_FAILURE_SMTP_PORT`,
  `CODEX_VOICE_FAILURE_SMTP_SECURITY`,
  `CODEX_VOICE_FAILURE_SMTP_USERNAME`, and
  `CODEX_VOICE_FAILURE_SMTP_PASSWORD`. Recipient and values remain external and
  are available only to the failure step.

## Exact companion Saga Ops requirements

1. Define selected service `codex-voice` at
   `/opt/saga-services/codex-voice`, immutable-artifact-only, with no source
   build path.
2. Strictly validate filename, digest, canonical manifest,
   service/commit/platform identity, member set, modes, and executable before
   install.
3. Install the validated executable directly at
   `/opt/saga-services/codex-voice/codex-voice` through a same-filesystem
   temporary file and atomic rename; do not add a release selector, `current`
   pointer, or payload transaction. Reject any target other than `codex-voice`
   and record the installed commit/digest in the deployment result.
4. Install a systemd service whose effective bind is `127.0.0.1:3845`, with
   application auth disabled and no non-loopback socket.
5. Add protected inputs for the one resolved config and provider environment,
   with schema/key/mode/owner/fingerprint checks and secret-safe output. Keep
   Saga Codex auth host-owned and check it read-only under the unit identity.
6. Add forward acceptance for active state, loopback health, embedded assets,
   Codex auth usability, config/provider readiness, and bounded
   transcription/TTS checks. Add no rollback-drill or rollback-acceptance gate.
   Emit the exact bounded `codex-voice-installed-service/v1` attestation below;
   producer acceptance must consume it through
   `scripts/accept_installed_service.py` rather than a source-tree test.
7. Add a serialized Gitea consumer accepting immutable artifact identity from a
   successful main producer run and automatically invoking only this service
   after private-canary approval.
8. Connect terminal failure to the existing Bex operational mail seam using the
   redacted contract. Recipient and credentials remain external configuration.
9. Keep Caddy cutover, canonical-host acceptance, and Asgard stop/disable as the
   separately authorized follow-up after automatic delivery is proven. The
   private SSH-forward canary is the prerequisite for enabling that delivery.

The Saga host attestation is JSON with exactly these fields:

```json
{
  "schema_version": "codex-voice-installed-service/v1",
  "service_id": "codex-voice",
  "source_commit": "<40-char-lowercase-commit>",
  "artifact_sha256": "<archive-sha256>",
  "artifact_binary_sha256": "<validated-extracted-binary-sha256>",
  "installed_binary_sha256": "<fixed-root-binary-sha256>",
  "version": "codex-voice <version>",
  "unit": "codex-voice.service",
  "unit_user": "ubuntu",
  "active_state": "active",
  "sub_state": "running",
  "listener": "127.0.0.1:3845",
  "service_instance_id": "<32-char-lowercase-process-instance-id>",
  "config_sha256": "<protected-config-sha256>",
  "config_ready": true,
  "provider_environment_ready": true,
  "codex_auth_ready": true
}
```

Saga generates it only after hashing the validated archive member and installed
fixed-root binary, running `--version` under the final identity, proving the
exact unit/listener, validating the one config and separately owned provider
environment without values, and checking existing Codex auth read-only under
`ubuntu`. The host verifier must resolve the process owning `127.0.0.1:3845`,
prove its executable digest equals the installed-binary digest, read that
process's `/healthz` instance ID, and confirm the listener owner and instance ID
remain unchanged across those checks. The acceptance program requires the same
instance ID through the second client's SSH forward, equality of artifact and
installed binary digests, then performs health/PWA/config/TTS/transcription
calls over the supplied loopback or SSH-forward URL. Its JSON output contains
only digests, version, booleans, provider names/counts, response sizes, and
transcript length; it never emits config, environment, auth, audio, or
transcript content.

## Remaining evidence and decisions

- Phase 1 confirmed the central selected-service task/variable names, fixed-root
  atomic replacement, anonymous package read, protected paths, Saga auth, and
  installed acceptance on the real service.
- Before publishing/activating the Phase 2 workflow, provision the protected
  controller provider environment at the fixed path above and configure the
  seven failure-email secrets. No suitable configured host mail transport was
  found, so these are real activation prerequisites rather than inferred state.
- Commit/push approval and one real successful-main automatic-delivery proof are
  still required. Caddy/domain cutover, the tray endpoint switch, Asgard backend
  retirement, and the deferred deployment-skill audit remain later work.

## Smallest implementation-ready sequence

1. Land the producer installed-service acceptance harness, exact Saga
   attestation contract, and post-verification Gitea package publication/read-back
   job. Only its publish step receives `PACKAGE_TOKEN`.
2. Complete Saga activation/attestation using the existing fixed-root installer;
   resolve protected input sources and package-read authorization without
   exposing values.
3. Publish/read back one immutable artifact and pass the real private
   SSH-forward canary.
4. Add and prove successful-main publication plus serialized Saga deployment,
   the same installed acceptance, and terminal failure-only email.
5. Stop with automatic delivery proven. In the separately authorized follow-up,
   change central Codex Voice ownership plus the generated Caddy fallback from
   `100.120.202.119:3845` to `127.0.0.1:3845`, accept the canonical host, then
   stop/disable Asgard `codex-voice-server.service` only after acceptance and
   prove its old listener/process is absent before removing Asgard ownership
   references.
