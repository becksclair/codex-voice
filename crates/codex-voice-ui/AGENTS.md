# codex-voice-ui

## Package Identity

`codex-voice-ui` owns presentation-facing status mapping plus the Linux tray, notification HUD, and settings/status window. Future cross-platform UI work should grow from here instead of leaking UI state into core or app wiring.

## Setup & Run

```bash
cargo check -p codex-voice-ui
cargo test -p codex-voice-ui
cargo clippy -p codex-voice-ui --all-targets -- -D warnings
```

## Patterns & Conventions

- Keep UI-facing types small and derived from core state.
- ✅ DO: Reuse `DictationState` from `crates/codex-voice-core/src/engine.rs`, as `UiStatus` does in `src/lib.rs`.
- ✅ DO: Map core `AppEvent` values to display labels in `src/lib.rs` instead of formatting them in app wiring.
- ✅ DO: Keep Linux tray code behind `cfg(target_os = "linux")`, as `StatusTray` does in `src/lib.rs`.
- ✅ DO: Keep menu commands as `UiCommand` values and handle runtime effects in `crates/codex-voice-app/src/main.rs`.
- ✅ DO: Add future Slint-facing wrappers here when they are presentation-specific.
- ✅ DO: Keep settings/HUD labels and display-specific formatting out of `codex-voice-core`.
- ❌ DON'T: Add platform permission or hotkey code here; use `crates/codex-voice-platform`.
- ❌ DON'T: Add transcription/auth logic here; use `crates/codex-voice-codex`.

## Touch Points / Key Files

- UI status mapping, Linux tray, notification HUD, and settings/status window: `src/lib.rs`
- Core state source: `crates/codex-voice-core/src/engine.rs`
- Future UI milestone: `docs/execplan-rust-native-cross-platform.md`

## JIT Index Hints

```bash
rg -n "UiStatus|StatusTray|UiCommand|HudWindow|SettingsWindow|DictationState" src ../codex-voice-core/src
rg -n "Slint|HUD|tray|settings" ../../docs/execplan-rust-native-cross-platform.md ../../README.md
rg -n "AppEvent|StateChanged" ../codex-voice-core/src/engine.rs ../codex-voice-app/src/main.rs
```

## Common Gotchas

- This crate intentionally has no Slint dependency yet; the current Linux surface uses GTK plus `tray-icon`, and `notify-send` for focus-safe HUD notifications.
- Keep GTK/AppIndicator dependencies target-scoped to Linux.
- Do not put runtime side effects behind GTK callbacks; send `UiCommand` to the app crate.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-ui && cargo clippy -p codex-voice-ui --all-targets -- -D warnings
```
