# codex-voice-platform

## Package Identity

`codex-voice-platform` contains OS adapter implementations behind core platform traits. The current implementation is Linux/KDE/Wayland-first, with portal diagnostics, GlobalShortcuts hotkeys, and RemoteDesktop keyboard paste.

## Setup & Run

```bash
cargo check -p codex-voice-platform
cargo test -p codex-voice-platform
cargo clippy -p codex-voice-platform --all-targets -- -D warnings
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "codex voice portal paste test"
```

## Patterns & Conventions

- Linux public adapters live in `src/linux.rs`; portal helpers live in `src/linux_remote_desktop.rs`, `src/linux_token_store.rs`, and `src/linux_clipboard.rs`.
- `LinuxPermissionService` reports portal availability through `busctl`.
- `LinuxHotkeyService` binds Control-M through `ashpd` GlobalShortcuts and emits press/release from portal activation signals.
- `LinuxTextInjector` sets the clipboard through `wl-copy`/`wl-paste` on Wayland, sends Ctrl+V through the RemoteDesktop keyboard portal, restores the prior clipboard, and reports `InsertMethod::PortalPaste`.
- ✅ DO: Keep real portal diagnostics precise with interface names like `org.freedesktop.portal.GlobalShortcuts`.
- ✅ DO: Restore the prior clipboard even if portal paste execution fails; see `insert_text()`.
- ✅ DO: Reuse persisted RemoteDesktop restore tokens via `src/linux_token_store.rs`.
- ✅ DO: Keep Linux-only code in `linux.rs` and expose unsupported behavior through `src/lib.rs` for other targets.
- ❌ DON'T: Reintroduce `wtype`/`ydotool` command fallbacks for KDE/Wayland input; use the permission-mediated portal path.
- ❌ DON'T: Add app CLI printing here; diagnostics should be returned as structured core types and printed in `codex-voice-app`.

## Touch Points / Key Files

- Linux adapter: `src/linux.rs`
- RemoteDesktop keyboard session: `src/linux_remote_desktop.rs`
- Persisted portal token storage: `src/linux_token_store.rs`
- Wayland clipboard handling: `src/linux_clipboard.rs`
- Platform trait definitions: `crates/codex-voice-core/src/platform.rs`
- App diagnostics: `crates/codex-voice-app/src/main.rs`
- Portal requirements: `docs/execplan-rust-native-cross-platform.md`

## JIT Index Hints

```bash
rg -n "LinuxPermissionService|portal_status|portal_version|busctl" src/linux.rs
rg -n "LinuxHotkeyService|GlobalShortcuts|Activated|Deactivated|Control-M" src/linux.rs
rg -n "LinuxTextInjector|Clipboard|PortalPaste|send_paste_chord" src/linux.rs src/linux_remote_desktop.rs
rg -n "LinuxClipboard|wl-copy|wl-paste|ClipboardSnapshot" src/linux_clipboard.rs
rg -n "PortalTokenStore|restore_token|XDG_STATE_HOME|portal-tokens" src/linux_token_store.rs src/linux_remote_desktop.rs
rg -n "InsertMethod|PermissionKind|PermissionStatus|TextInjector" ../codex-voice-core/src/platform.rs
```

## Common Gotchas

- This host is KDE/Wayland; `doctor linux-portals` should report GlobalShortcuts and RemoteDesktop versions when portals are available.
- The first paste may show a RemoteDesktop approval prompt; later process starts should reuse `~/.local/state/codex-voice/portal-tokens.json` when the portal returns a restore token.
- Clipboard restoration may fail; return the boolean in `InsertReport` rather than retrying indefinitely.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-platform && cargo clippy -p codex-voice-platform --all-targets -- -D warnings && cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
```
