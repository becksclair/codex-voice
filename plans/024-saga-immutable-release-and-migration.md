# Saga immutable release and migration

Status: **IN PROGRESS — producer slice implemented locally; nothing published or deployed.**

This plan separates confirmed current state from the approved target. It is the
release/cutover contract for moving Codex Voice from Asgard to Saga.

## Confirmed current state

- A release must build `web/dist` before Rust: cargo otherwise embeds a stub PWA.
- The application server is no-auth by default. It allows loopback and Tailnet
  binds; the migrated Saga service will use loopback only.
- Saga currently terminates Tailnet-only `voice.heliasar.com` ingress and proxies
  to Asgard. Asgard remains live owner until a separately authorized cutover.
- Saga uses a central Ansible, fixed-root, selected-service deployment lane.
- Saga has normal Codex auth already. This migration does not copy auth or log in.
- The one active Codex Voice config source/destination must still be resolved
  from the effective Asgard and Saga service definitions.
- No repository-local Bex mail recipient/credential seam is confirmed. The
  deploy workflow must use the existing operational notification interface.

## Approved artifact and automation contract

Every successful `main` push will eventually run two bounded workflows:

1. Build the Vite app and locked release binary, validate it, and publish an
   immutable artifact under the full source commit SHA.
2. Automatically consume that exact published artifact and deploy only the
   `codex-voice` selected service on Saga. Deployment never rebuilds source or
   resolves a mutable `latest` artifact.

The automatic deploy trigger is enabled only after the one-time private Saga
canary and canonical cutover bootstrap below.

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
2. **Canonical Tailnet-only cutover, separately authorized.** Change Saga
   Caddy's upstream from Asgard to Saga loopback without widening exposure.
   Accept DNS/TLS, PWA/assets, health, transcription, and TTS through the
   canonical hostname from a Tailnet consumer.
3. **Ownership closeout.** Only after live acceptance, stop/disable Asgard and
   remove its ownership references. Then enable recurring automatic deploy.

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
- **Artifact read credential:** reuse the Gitea package/artifact read credential
  owned by the selected-service lane; its configuration name remains evidence
  to resolve.
- **Saga deploy credential:** reuse the selected-service Ansible/SSH credential
  seam; its configuration name remains evidence to resolve.
- **Failure email:** reuse the existing Bex recipient, sender, and mail
  credential configuration. Those names/interfaces remain evidence to resolve
  and must not become repository literals.

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
7. Add a Gitea consumer accepting immutable artifact identity from a successful
   main producer run and automatically invoking only this service after the
   bootstrap approval.
8. Connect terminal failure to the existing Bex operational mail seam using the
   redacted contract. Recipient and credentials remain external configuration.
9. Keep private SSH-forward canary, Caddy cutover, canonical acceptance, and
   Asgard retirement as separately authorized one-time tasks, outside recurring
   deployment.

## Remaining evidence and decisions

- Confirm current Saga Ops selected-service task/variable names and the
  fixed-root atomic file-replacement contract.
- Resolve the Gitea artifact storage/download interface and immutable identity
  fields.
- Resolve the effective Asgard config source and Saga destination.
- Confirm Saga's final unit user/group and auth readability under that identity.
- Locate the existing mail command/API, recipient configuration name, sender
  owner, and credential owner without exposing values.
- Bex must separately authorize private install/canary, Caddy/domain cutover,
  and Asgard retirement. Automatic deployment policy itself is decided.

## Smallest implementation-ready sequence

1. Land this producer packager, validator tests, runtime smokes, and contract.
2. Add the matching Saga Ops consumer and protected-state preflights without
   enabling recurring deployment.
3. Build/publish one immutable artifact, then perform the private Saga canary.
4. With separate approval, cut over canonical ingress and accept consumers.
5. Retire Asgard ownership, then enable automatic deployment after every
   successful main artifact.
