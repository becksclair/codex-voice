# codex-voice-ui

## Package Identity

`codex-voice-ui` owns presentation-facing status mapping plus the native tray, notification HUD, and settings/status window for Linux, macOS, and Windows (`src/linux_tray.rs`, `src/macos_tray.rs`, `src/windows_tray.rs`). Keep UI state here instead of leaking it into core or app wiring.

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
- ✅ DO: Keep settings/HUD labels and display-specific formatting out of `codex-voice-core`.
- ❌ DON'T: Add platform permission or hotkey code here; use `crates/codex-voice-platform`.
- ❌ DON'T: Add transcription/auth logic here; use `crates/codex-voice-codex`.

## Touch Points / Key Files

- UI status mapping, Linux tray, notification HUD, and settings/status window: `src/lib.rs`
- macOS tray: `src/macos_tray.rs`
- Windows tray: `src/windows_tray.rs`
- Core state source: `crates/codex-voice-core/src/engine.rs`
- Cross-platform UI decision (per-platform native UI kept; Slint evaluated and not adopted): `ROADMAP.md` (Phase 6)

## JIT Index Hints

```bash
rg -n "UiStatus|StatusTray|UiCommand|HudWindow|SettingsWindow|DictationState" src ../codex-voice-core/src
rg -n "Slint|HUD|tray|settings" ../../ROADMAP.md ../../README.md
rg -n "AppEvent|StateChanged" ../codex-voice-core/src/engine.rs ../codex-voice-app/src/main.rs
```

## Common Gotchas

- This crate has no Slint dependency: per `ROADMAP.md` Phase 6, Slint was evaluated and not adopted; the Linux surface uses GTK plus `tray-icon` and `notify-send` for focus-safe HUD notifications, and macOS/Windows use `tray-icon` with platform-native dialogs and notifications.
- Keep GTK/AppIndicator dependencies target-scoped to Linux.
- Do not put runtime side effects behind GTK callbacks; send `UiCommand` to the app crate.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-ui && cargo clippy -p codex-voice-ui --all-targets -- -D warnings
```
