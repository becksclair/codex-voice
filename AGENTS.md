# Codex Voice Agent Guide

## Project Snapshot

Codex Voice is a Rust workspace for a Linux-first, Rust-native hold-to-dictate desktop utility. The workspace is split into small crates for app wiring, core state, audio capture, Codex auth/transcription, TTS, platform adapters, and UI. Read the nearest crate-level `AGENTS.md` before editing files under `crates/**`.

## Root Setup Commands

```bash
cargo fetch
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p codex-voice-app --bin codex-voice -- --version
```

Linux smoke checks:

```bash
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
timeout 10s cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 1
cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello"
```

## Universal Conventions

- Use Rust 2021 and workspace-managed dependency versions from `Cargo.toml`.
- Keep crates small and boundary-focused; shared contracts live in `crates/codex-voice-core`.
- Prefer typed errors in library crates and `anyhow::Result` only at app/CLI boundaries.
- Keep generated/runtime artifacts out of git; `target/` is ignored.
- For PWA/web assets, never reference immutable assets through bare stable URLs. This is now enforced structurally: only content-hashed `/web/assets/*` paths are served with immutable caching, and the app shell, service worker, manifests, and icons are served `no-cache`. Workbox content-hash revisions handle service-worker precache versioning, so keep new long-lived assets under the hashed `assets/` directory rather than reintroducing a build-revision query string.
- Update `README.md` and `ROADMAP.md` when command contracts change.
- Preserve Linux-first scope until portal hotkey/paste proof is complete.

## JS/Web Tooling Conventions

The standalone web frontend lives at `web/` (see `web/README.md`). Use `bun` as the package manager — never `npm` or `npx`; use `bunx` for one-off executables. Lint and format with oxlint and oxfmt, run unit tests with vitest, and keep TypeScript in strict mode. `mise run verify` includes the web gates (`web-check` = oxlint + oxfmt + tsc, and `web-test` = vitest), so a green `verify` covers the frontend as well as the Rust workspace.

## Security & Secrets

- Never print or commit access tokens, refresh tokens, full account IDs, full transcripts, or private audio.
- Codex auth is read from `~/.codex/auth.json`; do not write this file directly.
- TTS secrets are read from `~/.codex/read-aloud-defaults.json`; never log API keys.
- Diagnostics may print token presence, redacted account IDs, transcript length, and short previews only.
- Temp WAV files must be deleted unless the user explicitly asks to keep a diagnostic recording.

## Deployment Context

- This project is private and local-only, accessed behind Tailscale. The local transcriber service bearer token is for basic request identification within the trusted network, not for strong authentication.

## JIT Index

### Package Structure

- App/CLI wiring: `crates/codex-voice-app/` -> [see AGENTS.md](crates/codex-voice-app/AGENTS.md)
- Core traits/state machine: `crates/codex-voice-core/` -> [see AGENTS.md](crates/codex-voice-core/AGENTS.md)
- Audio capture: `crates/codex-voice-audio/` -> [see AGENTS.md](crates/codex-voice-audio/AGENTS.md)
- Codex auth/transcription: `crates/codex-voice-codex/` -> [see AGENTS.md](crates/codex-voice-codex/AGENTS.md)
- TTS (Google Gemini + ElevenLabs): `crates/codex-voice-tts/` -> [see AGENTS.md](crates/codex-voice-tts/AGENTS.md)
- Transcriber service/client/discovery: `crates/codex-voice-transcriber/` -> [see AGENTS.md](crates/codex-voice-transcriber/AGENTS.md)
- Platform adapters: `crates/codex-voice-platform/` -> [see AGENTS.md](crates/codex-voice-platform/AGENTS.md)
- Native tray/HUD/settings surfaces for Linux, macOS, and Windows: `crates/codex-voice-ui/` -> [see AGENTS.md](crates/codex-voice-ui/AGENTS.md)
- Architecture plan: `ROADMAP.md` (see `docs/execplan-rust-native-cross-platform.md.ARCHIVED` for original detailed research)

### Quick Find Commands

```bash
rg -n "DictationEngine|HotkeyEvent|TextInjector|AudioRecorder|TranscriptionClient" crates
rg -n "doctor|Parser|Subcommand" crates/codex-voice-app/src
rg -n "cpal|WavWriter|RecordedAudio" crates/codex-voice-audio/src
rg -n "auth|transcribe|TRANSCRIBE_URL|account/read" crates/codex-voice-codex/src
rg -n "tts|speech|synthesize|ProviderKind|FallbackPolicy|ReadAloudConfig" crates/codex-voice-tts/src
rg -n "GlobalShortcuts|RemoteDesktop|Clipboard|PortalTokenStore|PortalPaste" crates/codex-voice-platform/src
find crates -name '*test*' -o -name '*.rs'
```

## Definition of Done

- Relevant crate-level checks pass, then root `cargo fmt --check`, `cargo check --workspace`, and `cargo test --workspace`.
- Run `cargo clippy --workspace --all-targets -- -D warnings` after non-trivial Rust changes.
- After web frontend changes, run the web gates (`mise run web-check` and `mise run web-test`); `mise run verify` runs the full Rust + web gate set.
- After modifying any installed/runtime service behavior, rebuild and restart the affected user services before marking the task complete, then verify the restarted service is healthy.
- For Linux runtime changes, run `doctor linux-portals` and the 1-second `doctor audio` smoke when relevant.
- For TTS changes, run `doctor tts` with a short test phrase.
- Update README/ROADMAP for changed CLI, runtime, auth, platform, or TTS behavior.
