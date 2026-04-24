# codex-voice-ui

## Package Identity

`codex-voice-ui` is currently a minimal placeholder for UI-facing status types. Future Slint tray/settings/HUD work should grow from here instead of leaking UI state into core or app wiring.

## Setup & Run

```bash
cargo check -p codex-voice-ui
cargo test -p codex-voice-ui
cargo clippy -p codex-voice-ui --all-targets -- -D warnings
```

## Patterns & Conventions

- Keep UI-facing types small and derived from core state.
- ✅ DO: Reuse `DictationState` from `crates/codex-voice-core/src/engine.rs`, as `UiStatus` does in `src/lib.rs`.
- ✅ DO: Add Slint-facing wrappers here when they are presentation-specific.
- ✅ DO: Keep settings/HUD labels and display-specific formatting out of `codex-voice-core`.
- ❌ DON'T: Add platform permission or hotkey code here; use `crates/codex-voice-platform`.
- ❌ DON'T: Add transcription/auth logic here; use `crates/codex-voice-codex`.

## Touch Points / Key Files

- UI status placeholder: `src/lib.rs`
- Core state source: `crates/codex-voice-core/src/engine.rs`
- Future UI milestone: `docs/execplan-rust-native-cross-platform.md`

## JIT Index Hints

```bash
rg -n "UiStatus|DictationState" src ../codex-voice-core/src
rg -n "Slint|HUD|tray|settings" ../../docs/execplan-rust-native-cross-platform.md
rg -n "AppEvent|StateChanged" ../codex-voice-core/src/engine.rs ../codex-voice-app/src/main.rs
```

## Common Gotchas

- This crate intentionally has no Slint dependency yet.
- Do not invent UI architecture before the app has real tray/HUD requirements.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-ui && cargo clippy -p codex-voice-ui --all-targets -- -D warnings
```
