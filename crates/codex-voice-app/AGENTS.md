# codex-voice-app

## Package Identity

`codex-voice-app` is the CLI/runtime wiring crate. It exposes the `codex-voice` binary, diagnostic subcommands, Linux run loop, logging setup, and top-level error handling.

## Setup & Run

```bash
cargo run -p codex-voice-app --bin codex-voice -- --version
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 1
cargo run -p codex-voice-app --bin codex-voice -- doctor codex-auth
cargo run -p codex-voice-app --bin codex-voice -- doctor transcribe --file /tmp/sample.wav
cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "codex voice portal paste test"
cargo check -p codex-voice-app
```

## Patterns & Conventions

- CLI definitions live in `src/main.rs` using `clap` derive types (`Cli`, `Command`, `DoctorCommand`).
- Keep the public binary name `codex-voice` in `Cargo.toml`; the package remains `codex-voice-app`.
- ✅ DO: Add diagnostic commands by extending `DoctorCommand` in `src/main.rs` and wiring the match in `main()`.
- ✅ DO: Keep CLI parsing thin and delegate behavior to crate APIs, as `doctor_audio()` delegates to `CpalWavRecorder`.
- ✅ DO: Redact auth in diagnostics like `doctor_codex_auth()`; print token presence, never token values.
- ✅ DO: Print transcript length and a short preview only, following `doctor_transcribe()`.
- ❌ DON'T: Change the CLI shape without updating `README.md` and `docs/execplan-rust-native-cross-platform.md`; both document concrete `cargo run ... --bin codex-voice -- ...` commands.
- ❌ DON'T: Put platform-specific implementation details here; Linux-specific behavior belongs in `crates/codex-voice-platform/src/linux.rs`.
- Use `anyhow::Result` in this crate only; lower crates should expose typed result aliases.

## Touch Points / Key Files

- CLI and diagnostics: `src/main.rs`
- Binary name: `Cargo.toml`
- Core state integration: `crates/codex-voice-core/src/engine.rs`
- Audio diagnostics: `crates/codex-voice-audio/src/lib.rs`
- Codex diagnostics: `crates/codex-voice-codex/src/lib.rs`
- Linux platform diagnostics: `crates/codex-voice-platform/src/linux.rs`

## JIT Index Hints

```bash
rg -n "enum DoctorCommand|doctor_|Subcommand|Parser" src/main.rs
rg -n "codex-voice-app|--bin codex-voice|doctor " ../../README.md ../../docs
rg -n "AppEvent|DictationEngine|print_app_event" src/main.rs ../../crates/codex-voice-core/src
rg -n "redact|access_token|preview|transcript_chars" src/main.rs
```

## Common Gotchas

- `doctor transcribe` requires `--file`; do not change it back to a positional file without updating docs.
- `doctor paste` requires `--text`; this is intentionally documented in the ExecPlan.
- `run` binds Control-M through the Linux GlobalShortcuts portal; approval may be prompted by the desktop.
- Keep Linux-only commands behind `#[cfg(target_os = "linux")]`.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-app && cargo test -p codex-voice-app && cargo run -q -p codex-voice-app --bin codex-voice -- --version
```
