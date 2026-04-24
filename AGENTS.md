# Codex Voice Agent Guide

## Project Snapshot

Codex Voice is a Rust workspace for a Linux-first, Rust-native hold-to-dictate desktop utility. The workspace is split into small crates for app wiring, core state, audio capture, Codex auth/transcription, platform adapters, and UI placeholders. Read the nearest crate-level `AGENTS.md` before editing files under `crates/**`.

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
```

## Universal Conventions

- Use Rust 2021 and workspace-managed dependency versions from `Cargo.toml`.
- Keep crates small and boundary-focused; shared contracts live in `crates/codex-voice-core`.
- Prefer typed errors in library crates and `anyhow::Result` only at app/CLI boundaries.
- Keep generated/runtime artifacts out of git; `target/` is ignored.
- Update `README.md` and `docs/execplan-rust-native-cross-platform.md` when command contracts change.
- Preserve Linux-first scope until portal hotkey/paste proof is complete.

## Security & Secrets

- Never print or commit access tokens, refresh tokens, full account IDs, full transcripts, or private audio.
- Codex auth is read from `~/.codex/auth.json`; do not write this file directly.
- Diagnostics may print token presence, redacted account IDs, transcript length, and short previews only.
- Temp WAV files must be deleted unless the user explicitly asks to keep a diagnostic recording.

## JIT Index

### Package Structure

- App/CLI wiring: `crates/codex-voice-app/` -> [see crates/codex-voice-app/AGENTS.md](crates/codex-voice-app/AGENTS.md)
- Core traits/state machine: `crates/codex-voice-core/` -> [see crates/codex-voice-core/AGENTS.md](crates/codex-voice-core/AGENTS.md)
- Audio capture: `crates/codex-voice-audio/` -> [see crates/codex-voice-audio/AGENTS.md](crates/codex-voice-audio/AGENTS.md)
- Codex auth/transcription: `crates/codex-voice-codex/` -> [see crates/codex-voice-codex/AGENTS.md](crates/codex-voice-codex/AGENTS.md)
- Platform adapters: `crates/codex-voice-platform/` -> [see crates/codex-voice-platform/AGENTS.md](crates/codex-voice-platform/AGENTS.md)
- UI placeholder: `crates/codex-voice-ui/` -> [see crates/codex-voice-ui/AGENTS.md](crates/codex-voice-ui/AGENTS.md)
- Architecture plan: `docs/execplan-rust-native-cross-platform.md`

### Quick Find Commands

```bash
rg -n "DictationEngine|HotkeyEvent|TextInjector|AudioRecorder|TranscriptionClient" crates
rg -n "doctor|Parser|Subcommand" crates/codex-voice-app/src
rg -n "cpal|WavWriter|RecordedAudio" crates/codex-voice-audio/src
rg -n "auth|transcribe|TRANSCRIBE_URL|account/read" crates/codex-voice-codex/src
rg -n "GlobalShortcuts|RemoteDesktop|Clipboard|wtype|ydotool" crates/codex-voice-platform/src
find crates -name '*test*' -o -name '*.rs'
```

## Definition of Done

- Relevant crate-level checks pass, then root `cargo fmt --check`, `cargo check --workspace`, and `cargo test --workspace`.
- Run `cargo clippy --workspace --all-targets -- -D warnings` after non-trivial Rust changes.
- For Linux runtime changes, run `doctor linux-portals` and the 1-second `doctor audio` smoke when relevant.
- Update README/ExecPlan for changed CLI, runtime, auth, or platform behavior.
