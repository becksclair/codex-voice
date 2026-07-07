# codex-voice-ui

## Package Identity

`codex-voice-ui` owns presentation-facing status mapping plus the native tray, notification HUD, and settings/status window for Linux, macOS, and Windows (`src/linux_tray.rs`, `src/linux_windows.rs`, `src/macos_tray.rs`, `src/windows_tray.rs`). Keep UI state here instead of leaking it into core or app wiring.

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

- This crate has no Slint dependency: per `ROADMAP.md` Phase 6, Slint was evaluated and not adopted. The Linux surface uses `ksni` (StatusNotifierItem over D-Bus) for the tray, `iced` for the Settings/Speak Text windows, and `notify-send` for focus-safe HUD notifications — no GTK. macOS/Windows use `tray-icon` with platform-native dialogs and notifications.
- Keep `ksni`/`iced` dependencies target-scoped to Linux and `tray-icon` target-scoped to macOS/Windows.
- The Linux iced window daemon must own the process main thread (winit constraint); the engine/run-loop run on a background thread. See `src/linux_tray.rs` and `src/linux_windows.rs`.
- Do not put runtime side effects in ksni menu closures or iced update handlers; send `UiCommand` to the app crate (or `WindowEvent` to the daemon).

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-ui && cargo clippy -p codex-voice-ui --all-targets -- -D warnings
```
