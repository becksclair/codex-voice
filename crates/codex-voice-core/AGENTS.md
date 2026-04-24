# codex-voice-core

## Package Identity

`codex-voice-core` owns platform-neutral contracts and the dictation state machine. It should not know about CPAL, HTTP, D-Bus, clipboard tools, or CLI parsing.

## Setup & Run

```bash
cargo check -p codex-voice-core
cargo test -p codex-voice-core
cargo test -p codex-voice-core engine
cargo clippy -p codex-voice-core --all-targets -- -D warnings
```

## Patterns & Conventions

- Traits are split by boundary: `src/audio.rs`, `src/transcription.rs`, and `src/platform.rs`.
- State-machine behavior lives in `src/engine.rs`; keep side effects behind the injected traits.
- ✅ DO: Add new platform-neutral events to `AppEvent` in `src/engine.rs`.
- ✅ DO: Add new platform contracts to `src/platform.rs` when both app and adapter crates need them.
- ✅ DO: Use fake trait implementations in `src/engine.rs` tests for state-machine behavior.
- ✅ DO: Delete temp recordings after transcription attempts, as `process_recording()` does.
- ❌ DON'T: Import `cpal`, `reqwest`, `arboard`, `Command`, or OS APIs into this crate; see existing implementations in sibling crates instead.
- ❌ DON'T: Leave the engine stuck in `DictationState::Error`; `fail()` emits error state and returns to idle.
- Keep minimum recording behavior centralized with `MIN_RECORDING`.

## Touch Points / Key Files

- State machine: `src/engine.rs`
- Audio trait and `RecordedAudio`: `src/audio.rs`
- Platform traits/events: `src/platform.rs`
- Transcription trait/errors: `src/transcription.rs`
- Public exports: `src/lib.rs`

## JIT Index Hints

```bash
rg -n "trait .*Recorder|trait .*Client|trait .*Injector|trait .*Service" src
rg -n "DictationState|AppEvent|handle_hotkey|process_recording|fail" src/engine.rs
rg -n "RecordedAudio|InsertReport|PermissionStatus" src
rg -n "#\\[tokio::test\\]|FakeAudio|FakeTranscription|FakeInjector" src/engine.rs
```

## Common Gotchas

- Engine tests use `NamedTempFile`; avoid assertions that depend on files surviving deletion.
- `AppEvent::Error` and `StateChanged(Error)` are separate signals by design.
- Keep traits `Send + Sync` so app wiring can share implementations through `Arc`.

## Pre-PR Checks

```bash
cargo fmt --check && cargo test -p codex-voice-core && cargo clippy -p codex-voice-core --all-targets -- -D warnings
```
