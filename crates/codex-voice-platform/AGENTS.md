# codex-voice-platform

## Package Identity

`codex-voice-platform` contains OS adapter implementations behind core platform traits. The current implementation is Linux/KDE/Wayland-first, with portal diagnostics and temporary clipboard-command paste fallback.

## Setup & Run

```bash
cargo check -p codex-voice-platform
cargo test -p codex-voice-platform
cargo clippy -p codex-voice-platform --all-targets -- -D warnings
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "codex voice portal paste test"
```

## Patterns & Conventions

- Linux implementation lives in `src/linux.rs`; `src/lib.rs` re-exports Linux types behind `#[cfg(target_os = "linux")]`.
- `LinuxPermissionService` reports portal availability through `busctl`.
- `LinuxHotkeyService` is currently a terminal Enter simulation, not the final portal shortcut implementation.
- `LinuxTextInjector` currently uses clipboard plus `wtype`/`ydotool`; it reports `InsertMethod::ClipboardPaste`.
- ✅ DO: Keep real portal diagnostics precise with interface names like `org.freedesktop.portal.GlobalShortcuts`.
- ✅ DO: Restore the prior clipboard even if paste command execution fails; see `insert_text()`.
- ✅ DO: Keep Linux-only code in `linux.rs` and expose unsupported behavior through `src/lib.rs` for other targets.
- ❌ DON'T: Report `PortalPaste` until RemoteDesktop portal keyboard events are actually wired.
- ❌ DON'T: Add app CLI printing here; diagnostics should be returned as structured core types and printed in `codex-voice-app`.

## Touch Points / Key Files

- Linux adapter: `src/linux.rs`
- Platform trait definitions: `crates/codex-voice-core/src/platform.rs`
- App diagnostics: `crates/codex-voice-app/src/main.rs`
- Portal requirements: `docs/execplan-rust-native-cross-platform.md`

## JIT Index Hints

```bash
rg -n "LinuxPermissionService|portal_status|portal_version|busctl" src/linux.rs
rg -n "LinuxHotkeyService|HotkeyEvent|blocking_send|Enter" src/linux.rs
rg -n "LinuxTextInjector|Clipboard|send_paste_chord|wtype|ydotool" src/linux.rs
rg -n "InsertMethod|PermissionKind|PermissionStatus|TextInjector" ../codex-voice-core/src/platform.rs
```

## Common Gotchas

- This host is KDE/Wayland; `doctor linux-portals` should report GlobalShortcuts and RemoteDesktop versions when portals are available.
- `wtype` and `ydotool` are fallback diagnostics only, not the final Wayland permission-mediated path.
- Clipboard restoration may fail; return the boolean in `InsertReport` rather than retrying indefinitely.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-platform && cargo clippy -p codex-voice-platform --all-targets -- -D warnings && cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
```
