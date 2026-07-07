# 023 — Replace the GTK3 tray stack with ksni + iced on Linux

**Written against:** `3ddc31f` (2026-07-08). Verify drift before starting: if `crates/codex-voice-ui/src/linux_tray.rs` no longer matches the line references below, re-locate by symbol name before editing; if it has been substantially rewritten, STOP and report.

## Context

The Linux tray currently rides on `tray-icon 0.22`, whose only Linux backend is `libappindicator` (GTK3), and `codex-voice-ui` additionally depends on `gtk 0.18` directly for two small windows. The entire gtk-rs 0.18 stack is unmaintained upstream — it is the source of all eight RUSTSEC advisory ignores in `deny.toml` and the `libgtk-3-dev` CI dependency. GTK4 is not an option: GTK4 removed tray support entirely, and no GTK4 libappindicator exists.

Target: **ksni** (pure-Rust StatusNotifierItem over D-Bus — the native tray protocol on KDE Plasma, which is the primary desktop) for the tray, and **iced** (pure-Rust GUI) for the two windows. Outcome: zero GTK anywhere in the dependency tree, all eight `deny.toml` advisory ignores deleted, `libgtk-3-dev` removed from CI.

Operator decisions already made:
- Windows are reimplemented in iced (operator chose iced explicitly over shell-out dialogs or routing to the web PWA).
- The notification HUD stays on `notify-send` (it has no GTK dependency).

## The one architectural constraint that shapes everything

**iced's event loop must run on the main thread.** iced 0.14 builds its winit event loop with no `with_any_thread` hook, and winit 0.30 refuses off-main-thread creation on Linux (declined upstream: iced-rs/iced #996, #602). Today the main thread runs the tokio `run_app` loop and the tray owns a background thread. This plan **inverts** that on Linux:

- main thread → `iced::daemon(...).run()` (starts with zero windows, opens them on demand)
- background thread → a tokio runtime running everything `run()` does today (engine, `run_app` select loop)
- ksni tray → its own small thread + `current_thread` tokio runtime inside `StatusTray::start` (keeps the sync `start` contract; ksni is tokio-native)

This inversion applies ONLY to the Linux `codex-voice run` path. The `server` subcommand and all other CLI paths never touch iced/ksni. macOS/Windows keep their current structure and keep `tray-icon`.

## Current state (verified 2026-07-08)

### `crates/codex-voice-ui/src/linux_tray.rs` (378 lines) — full rewrite target
- `LinuxUiConfig { log_path: PathBuf }` (:20-23)
- `StatusTray` (:25-29): `status_tx: Sender<UiStatus>`, `command_rx: Receiver<UiCommand>`, `_thread: JoinHandle<()>` — three `std::sync::mpsc` channels; `start()` (:32-50) spawns the tray thread and blocks on a `ready_rx` one-shot so init errors surface synchronously.
- **Frozen public surface** (fn-pointer contract test `tray_common.rs:119-128` — must keep compiling unchanged):
  `start(UiStatus, LinuxUiConfig) -> Result<Self, UiError>` · `update(&self, UiStatus)` · `try_recv_command(&self) -> Option<UiCommand>` · `status_sender(&self) -> Sender<UiStatus>`
- GTK usage: `gtk::init()` (:89), manual pump `while gtk::events_pending() { gtk::main_iteration_do(false) }` + 50 ms sleep loop (:131-171), menu via `tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem}` with `MENU_*` id consts (:91-113), icon via `tray_common::build_icon_cache()`.
- `HudWindow` (:174-237): shells out to `notify-send` (`--print-id`/`--replace-id` coalescing, per-state timeouts/urgency). **No GTK. Keep as-is.**
- `SettingsWindow` (:239-308): `gtk::Window` 460×280 — heading, live `status_label`, five static info rows (hotkeys/insertion/transcription/timeout/debug-logs), log path from `config.log_path`. Handled locally in the tray thread (no UiCommand).
- `SpeakTextDialog` (:310-378): `gtk::Window` 520×360 — scrolled TextView + Generate/Play/Close; Generate sends `UiCommand::SpeakText(text)`, Play sends `UiCommand::PlayLastSpeech` via a cloned `command_tx`.

### `crates/codex-voice-ui/src/tray_common.rs` — shared contract (plan 012)
- `MENU_STATUS/TEST_RECORDING/SPEAK_TEXT/SETTINGS/LOGS/DIAGNOSTICS/QUIT` consts (:12-18), `ICON_SIZE: u32 = 32` (:19)
- `UiCommand { StartTestRecording, SpeakText(String), PlayLastSpeech, OpenLogs, RunDiagnostics, Quit }` (:21-29); `UiError { TrayInit, Icon, EventLoop }` (:37-48)
- Icons are **runtime-drawn RGBA circles**, not assets: `build_icon_for_state` (:78-106) — 32×32 filled circle, per-state colors Idle `#5c6670`, Recording `#db3636`, Transcribing `#2b7fd3`, Inserting `#f2b84b`, Error `#cc241d`, radius `ICON_SIZE/2 - 2`, into `tray_icon::Icon::from_rgba`. `build_icon_cache`/`icon_for_state` (:50-76) wrap it.
- `UiStatus` in `lib.rs:3-67`: `{ state: DictationState, message: String }`, `tray_label()`, `title()`, `from_app_event`.

### App wiring — `crates/codex-voice-app`
- `main.rs` `start_tray<C>` (:136-147) — tray is optional; on error logs a warning and returns `None` (headless degrade). `TrayStart<C>` trait (:151-183), Linux impl :156-163. `run()` (Linux, :185-204) builds `LinuxUiConfig { log_path }`, starts tray, spawns engine, calls `run_app`.
- `app.rs`: `TrayHandle` trait (:25-41) — `try_recv_command`/`update`/`status_sender`; run loop (:63-135) is one `tokio::select!` with a 200 ms `tray_poll` interval draining `try_recv_command`. `UiCommand::Quit` returns from `run_app`. No explicit tray teardown (thread detached).
- App tests use `FakeTray` (`app.rs:558-708`) — no real tray; unaffected.

### Cargo / CI / deny
- Workspace: `tray-icon = { version = "0.22.1", default-features = false }` (Cargo.toml:51). ui crate: `tray-icon.workspace = true`, Linux-only `gtk = "0.18"`. Even with `default-features = false`, tray-icon pulls libappindicator + muda + full gtk 0.18 stack on Linux. **Nothing else in the workspace links GTK** (codex-voice-platform uses ashpd portals, not GTK).
- `deny.toml` advisories ignore list: RUSTSEC-2024-0412/0413/0415/0416/0418/0419/0420 (gtk-rs) + RUSTSEC-2024-0370 (proc-macro-error via gtk3-macros) — all become deletable.
- CI (`.github/workflows/ci.yml:19` and identical `.gitea/...`): `libgtk-3-dev` in apt deps — becomes deletable (keep `libasound2-dev libxdo-dev pkg-config`).

## Library facts (researched 2026-07-08 — verify versions at execution)

### ksni 0.3.5 (active; 0.3.5 released 2026-06-10; tokio is the DEFAULT feature)
- Entry: `ksni::TrayMethods` blanket trait → `tray.spawn().await -> Result<Handle<T>, ksni::Error>`; spawns onto the ambient tokio runtime.
- `ksni::Tray` trait: required `fn id(&self) -> String`; override `title()`, `icon_pixmap() -> Vec<ksni::Icon>`, `tool_tip() -> ToolTip`, `menu() -> Vec<MenuItem<Self>>`, `watcher_offline(&self, OfflineReason) -> bool` (return `true` to survive plasmashell restarts and re-register on `watcher_online`), and `const MENU_ON_ACTIVATE: bool` — **set `true`** to preserve appindicator-style left-click-opens-menu.
- Menu model is inverted vs tray-icon: no ids/event channel; items carry closures `activate: Box<dyn Fn(&mut T) + Send>` mutating the tray struct; the menu re-renders as a pure function of state. Store channel senders IN the tray struct and send from closures.
- `Handle<T>` is Clone: `async fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R>` (triggers D-Bus updates), `shutdown()`, `is_closed()`.
- **Icon format: ARGB32, network byte order** — convert the existing RGBA buffers with `for px in data.chunks_exact_mut(4) { px.rotate_right(1) }`. `ksni::Icon { width: i32, height: i32, data: Vec<u8> }`.
- Failure modes: `spawn()` errs with `Error::Watcher(_)` (no SNI support), `Error::WontShow` (no StatusNotifierHost yet), `Error::Dbus(_)` — map to `UiError::TrayInit` with the friendly message; the enum is `#[non_exhaustive]`, match with a wildcard.

### iced 0.14 (released 2026-06-12; NOT 0.13 — the daemon signature changed)
- `iced::daemon(boot, update, view)` — boot first (0.13 took title first); `.title(fn(&State, window::Id) -> String)`, `.subscription(...)`, `.theme(...)`, `.run()`. **Starts with zero windows; does not exit when all windows close; exits only via `iced::exit` task. `.run()` blocks the main thread.**
- `window::open(window::Settings) -> (window::Id, Task<window::Id>)`, `window::close::<T>(id) -> Task<T>`, `window::close_events() -> Subscription<window::Id>` (prune state on user close; tolerate a possibly-missed final close event on Wayland — iced #3229), `window::gain_focus(id)`.
- `view(state, id)` routes per window by matching stored `window::Id`s.
- Features: `default-features = false, features = ["tokio", "tiny-skia", "x11", "wayland", "linux-theme-detection", "fira-sans"]` — **no wgpu**; tiny-skia software rendering is plenty for two small windows and avoids ~100 transitive GPU crates.
- External events in: `Subscription::run(f)` where `f` is a capture-free `fn() -> impl Stream<Item = Message>`; hand the receiver over via a `static OnceLock<Mutex<Option<Receiver<...>>>>` taken inside the stream fn (or `Subscription::run_with` — verify availability at execution). Multiline input: `iced::widget::text_editor` with `text_editor::Content` state, `.on_action(Message::Edit)`, `content.perform(action)`, `content.text()`.
- iced's `tokio` feature makes its executor tokio-backed with its OWN internal runtime — coexists fine with the app's runtime on another thread; bridge with channels only.

## Target design

### Channel topology (Linux `run` path)
```
main thread                     background thread                tray thread
┌─────────────────┐   WindowEvent   ┌──────────────────┐  UiStatus   ┌─────────────────┐
│ iced daemon      │◄───(tokio      │ tokio runtime:    │───(std     │ ksni service     │
│ (zero windows,   │    unbounded)──│ engine + run_app  │   mpsc)───►│ (current_thread  │
│ Settings +       │                │ select loop       │            │ runtime)         │
│ SpeakText views) │────UiCommand───│ (200ms tray_poll) │◄─UiCommand─│ menu closures    │
└─────────────────┘   (std mpsc     └──────────────────┘  (std mpsc) └─────────────────┘
                       command_tx clone)                        └── WindowEvent tx too
```
- `WindowEvent { OpenSettings, OpenSpeakText, Status(UiStatus), Exit }` — new enum in `codex-voice-ui`. Sent by: tray menu closures (OpenSettings/OpenSpeakText), the status forwarder (Status — so an open settings window shows live status), and the app thread on shutdown (Exit → iced update returns `iced::exit()`).
- `UiCommand` senders: ksni closures (StartTestRecording/OpenLogs/RunDiagnostics/Quit/PlayLastSpeech) and iced's SpeakText window (SpeakText(text), PlayLastSpeech). Both are clones of the same std mpsc `command_tx`; `run_app`'s existing 200 ms poll is unchanged.

### `StatusTray` (Linux) — same frozen 4-method surface
`start()` spawns one thread that builds a `tokio::runtime::Builder::new_current_thread().enable_all()` runtime, constructs `KsniTray { status: UiStatus, command_tx, window_tx, icons: HashMap<DictationState, ksni::Icon> }`, runs `tray.spawn().await` (ready channel reports success/`UiError::TrayInit`), then loops: `status_rx.recv()` (blocking std mpsc) → `block_on(handle.update(|t| t.status = s))` → also forward `WindowEvent::Status`. Thread ends when `status_rx` disconnects → `block_on(handle.shutdown())`.
- Menu (pure fn of state, same order as today): disabled status item (`tray_label()`), separator, Start Test Recording, Speak text…, Open Settings, Open Logs, Run Diagnostics, separator, Quit. `MENU_ON_ACTIVATE = true`. Tooltip: `ToolTip { title: "Codex Voice", description: status.message }`.
- `watcher_offline` → `true` (survive plasmashell restarts).

### Icon helpers (`tray_common.rs`) — split pure pixels from platform types
- New shared pure fn: `icon_rgba_for_state(state: &DictationState) -> Vec<u8>` (the existing circle-drawing loop, minus `tray_icon::Icon::from_rgba`) + unit test asserting center/corner pixel colors per state.
- `build_icon_cache`/`icon_for_state`/`build_icon_for_state` (returning `tray_icon::Icon`) move behind `#[cfg(not(target_os = "linux"))]` — macOS/Windows keep them. Linux gets `ksni_icon_for_state` (RGBA → ARGB `rotate_right(1)`, `ksni::Icon`) in `linux_tray.rs` or a linux-only helper module, with a unit test asserting the byte reorder (input RGBA `[r,g,b,a]` → `[a,r,g,b]`).
- `MENU_*` consts stay (used by macOS/Windows); Linux stops using them (ksni has no ids) — expect a dead-code lint if they were Linux-referenced only via the old file; they are used by both other platforms, so no change needed.

### iced daemon (`crates/codex-voice-ui/src/linux_windows.rs`, new)
- `pub fn run_window_daemon(events: tokio::sync::mpsc::UnboundedReceiver<WindowEvent>, command_tx: std::sync::mpsc::Sender<UiCommand>, info: SettingsInfo) -> iced::Result` — called by the app on the main thread. `SettingsInfo { log_path: PathBuf }` (+ the five static info strings, lifted verbatim from `SettingsWindow::new`).
- State: `settings_win: Option<window::Id>`, `speak_win: Option<window::Id>`, `status: UiStatus`, `content: text_editor::Content`.
- Update handles: `WindowEvent::OpenSettings/OpenSpeakText` (open or `gain_focus` if already open), `Status` (refresh label), `Exit` → `iced::exit()`; `WindowClosed(id)` from `window::close_events()` prunes; Speak button → `command_tx.send(UiCommand::SpeakText(content.text()))`, Play → `PlayLastSpeech`, Close → `window::close(id)`.
- Views: settings = column of labels (heading, live `Status: {message}`, the five info rows, log path — content parity with `SettingsWindow` :246-306); speak = `text_editor` + Generate/Play/Close row (parity with `SpeakTextDialog` :310-378).
- Receiver into the subscription via the `OnceLock` handoff pattern (or `run_with` if available).

### App entry (`crates/codex-voice-app/src/main.rs`, Linux path only)
- `run()` (Linux) restructures to: create the `WindowEvent` channel → spawn background thread { build multi-thread tokio runtime → existing body: logging, config, `start_tray` (now passing `window_tx` inside `LinuxUiConfig`), engine, `run_app` → on return, send `WindowEvent::Exit` } → main thread calls `run_window_daemon(...)` → after it returns, join the background thread and propagate its result.
- **Degrade path**: if `run_window_daemon` errors immediately (no display / winit failure), log a warning and fall back to joining the background thread — the app keeps running tray-only or fully headless, matching today's optional-tray philosophy. If the tray also failed, behavior equals today's headless mode.
- `LinuxUiConfig` grows `window_tx: tokio::sync::mpsc::UnboundedSender<WindowEvent>` (per-platform config types are NOT part of the frozen contract).
- `server` subcommand and non-Linux `run()` paths: untouched.

### Cargo
- Workspace: move `tray-icon` from `[workspace.dependencies]` used-by-all to target-gated in the ui crate: `[target.'cfg(any(target_os = "windows", target_os = "macos"))'.dependencies] tray-icon = ...` (keep workspace entry; gate the usage). Add `ksni = "0.3"` and `iced = { version = "0.14", default-features = false, features = [...] }` under `[target.'cfg(target_os = "linux")'.dependencies]` of `codex-voice-ui`. Delete `gtk = "0.18"`. App crate needs no new deps (channels come from tokio already in workspace).

### Cleanup payoff (same change set)
- `deny.toml`: delete all 8 advisory ignores (restore `ignore = []`); update the `[bans]` comment (GTK3 no longer contributes duplicates; symphonia still does).
- CI both workflows: drop `libgtk-3-dev` from apt.
- Docs: README (Linux tray section if it mentions GTK/appindicator), AGENTS.md JIT index unchanged, ROADMAP note, plans/README row.

## Steps (each with its own gate)

1. **Baseline**: `mise run verify` green at start; record `cargo tree -i gtk -e normal | head -1` output (proves GTK present pre-change).
2. **tray_common icon split** (small, cross-platform): extract `icon_rgba_for_state`, gate the `tray_icon::Icon` helpers to non-Linux, add pixel unit tests. Gate: `cargo test -p codex-voice-ui` green on Linux; `cargo check` must still pass for the other platforms' cfg (run `cargo check -p codex-voice-ui --target x86_64-pc-windows-msvc` only if the toolchain target is installed; otherwise note it for the Windows devbox verification).
3. **Cargo swap**: gate tray-icon, add ksni + iced (linux), remove gtk. Gate: `cargo check -p codex-voice-ui` (expect linux_tray.rs errors — proceed to 4 in the same commit or keep 3+4 as one logical commit).
4. **Rewrite `linux_tray.rs`**: KsniTray + StatusTray thread/runtime/forwarder + HudWindow kept + `WindowEvent` enum. Delete SettingsWindow/SpeakTextDialog GTK code. Gate: `cargo test -p codex-voice-ui` green (contract test compiles unchanged), clippy clean.
5. **`linux_windows.rs` iced daemon** + `main.rs` inversion + `LinuxUiConfig.window_tx`. Gate: `cargo test --workspace`, clippy, fmt.
6. **Cleanup**: deny ignores deleted → `cargo deny check advisories licenses sources` green; `cargo tree -i gtk -e normal` → "package ID specification `gtk` did not match any packages" (paste output in report); CI apt lines; docs.
7. **Full gates**: `mise run verify`, `mise run deny`, `mise run test-web` (should be untouched but proves no collateral).
8. **Operator smoke checklist** (write into the report; requires the human at the desktop):
   - `mise run setup` + relogin/restart `codex-voice.service`; tray icon appears in Plasma systray, correct idle color.
   - State colors change during a test recording (menu → Start Test Recording).
   - Menu: all seven items act; Speak text… opens the iced window; Generate speaks; Play replays; window close/reopen ×5 works (Wayland reopen check).
   - Open Settings shows live status while recording.
   - `systemctl --user restart plasma-plasmashell` → icon reappears within seconds (watcher_offline=true path).
   - Quit exits the app cleanly (iced daemon returns, background thread joins).

## Boundaries
- Do NOT touch: `macos_tray.rs`, `windows_tray.rs`, the `UiCommand`/`UiError` enums, the four frozen `StatusTray` method signatures, `app.rs` run-loop semantics (the 200 ms poll stays), `HudWindow`, anything under `web/`/`webtests/`/`crates/codex-voice-transcriber`, the `server` subcommand path.
- No new workspace-wide deps; ksni/iced are linux-target deps of `codex-voice-ui` only.
- Do not redesign the menu structure, labels, or window content — parity port.

## Done criteria (machine-checkable)
- `cargo tree -e normal | grep -iE 'gtk|glib|libappindicator|muda|atk|gdk'` → empty.
- `deny.toml` advisories `ignore = []` AND `cargo deny check advisories` green.
- `grep libgtk .github/workflows/ci.yml .gitea/workflows/ci.yml` → empty.
- `cargo test --workspace` green incl. `status_tray_surface_contract` unmodified.
- New unit tests: `icon_rgba_for_state` pixels, RGBA→ARGB reorder.
- `mise run verify` + `mise run deny` green.

## Maintenance notes
- ksni menu is a pure function of `KsniTray` state — future menu items = new field + closure, no id plumbing.
- iced 0.14 daemon: window state must be pruned via `close_events()`; tolerate a missed final close event on Wayland (iced #3229) by treating `open` on a stale id as reopen.
- The main-thread inversion is Linux-only; if macOS ever needs iced it has the same main-thread constraint but a different current structure — do not assume this plan generalizes.
- If tray-icon ever ships a non-GTK Linux backend, this plan's ksni layer could be revisited — unlikely to be worth it once ksni is in.

## Escape hatches (STOP and report instead of improvising)
- ksni `spawn()` fails on the dev machine outside of the known Watcher/WontShow cases, or the KDE tray misrenders ARGB icons after the documented `rotate_right(1)` conversion.
- iced 0.14's `Subscription::run` cannot accept the receiver handoff pattern AND `run_with` is absent — do not invent a polling bridge; report.
- The daemon inversion breaks a non-Linux build (cfg leakage into shared main.rs paths).
- `iced` with `tiny-skia`-only refuses to create windows on the dev machine's Plasma/Wayland session (renderer gap) — enabling `wgpu` as fallback is allowed but report the dependency-count delta first.
- Any change would require touching the frozen `StatusTray` contract or `FakeTray` app tests.
