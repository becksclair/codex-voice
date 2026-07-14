# Codex Voice — Roadmap

Rust-native, cross-platform hold-to-dictate desktop utility. Press and hold `Control-M`, speak, release, and the transcript is inserted into the focused application. Press `Super-F6` (`Command-F6` on macOS, `Win-F6` on Windows) to speak selected text through the local TTS service.

This roadmap replaces `@docs/execplan-rust-native-cross-platform.md` as the canonical plan of record.

---

## Phase 1 — Core Foundation

Platform-neutral dictation engine, audio capture, and Codex transcription backend.

- [x] Create Rust workspace with resolver 2 and shared dependency versions
- [x] Implement `DictationEngine` state machine (`Idle → Recording → Transcribing → Inserting → Idle/Error`)
  - Discards recordings shorter than 120 ms
  - Deletes temp recordings after transcription attempt
  - Returns to idle after successful insertion or error
- [x] Implement `AudioRecorder` trait with CPAL-backed mono 16-bit WAV capture
  - `@crates/codex-voice-audio/AGENTS.md`
- [x] Implement `TranscriptionClient` trait with Codex auth + private endpoint
  - Read `~/.codex/auth.json`; refresh via `codex app-server --listen stdio://`
  - Post multipart WAV to `chatgpt.com/backend-api/transcribe`
  - `@crates/codex-voice-codex/AGENTS.md`
- [x] Add unit tests for state transitions and short-recording discard
- [x] Add diagnostic commands: `doctor audio`, `doctor codex-auth`, `doctor transcribe`
- [x] Add `codex-voice --version`

**Validation:** `cargo test --workspace`, `cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 1`

---

## Phase 2 — Local Audio Service

OpenAI-compatible localhost service so tools like `summarize` can reuse Codex Voice without patching.

- [x] Implement `codex-voice server` with Axum
  - `POST /v1/audio/transcriptions` — OpenAI-compatible transcription
  - `POST /v1/audio/speech` — OpenAI-compatible TTS
  - `GET /v1/healthz` — health + capability flags
- [x] Implement service discovery file (`~/.local/state/codex-voice/transcriber.json`)
- [x] Implement `RuntimeTranscriptionClient` with Codex fallback
  - Probes local service at startup; falls back to direct Codex if stale/unhealthy
  - **Fixed 2026-05-23:** 5xx errors from local service now correctly retry through direct Codex; 4xx client errors remain non-retryable
- [x] Implement audio chunking with `ffmpeg` for oversized uploads
  - 24 MiB Codex backend limit, 512 MiB client upload limit
  - Returns `413 Payload Too Large` when chunking is unavailable
- [x] Implement TTS with Google Gemini and ElevenLabs backends
  - Config loaded from `~/.codex/read-aloud-defaults.json`
  - Persona-aware provider fallback
  - PCM → WAV wrapping in-process; compressed formats via `ffmpeg`
- [x] Add `doctor tts` diagnostic
- [x] Add `tts bench` speech-prep benchmark (replaces `scripts/tts_prep_benchmark.py`)
- [x] Add `transcriber probe-limits` for backend stress testing
- [x] Add `mise run setup` for Linux systemd user service install
- [x] Serve an installable TTS web app (PWA) from the same listener
  - **Migrated 2026-07-07:** the embedded single-file `app.html` was replaced by
    a standalone React frontend at `web/` (Vite, React, TypeScript, Tailwind,
    built with `bun`). `web/dist` is embedded at build time; `server --web-dist
    <dir>` serves a dist from disk for binary-independent web deploys. Only
    content-hashed `/web/assets/*` is immutable-cached. See `web/README.md`.
- [x] Add bounded speech-job admission and cancellation
  - At most three nonterminal jobs, one active synthesis, `429 Retry-After` on overload
  - `DELETE /web/speech-jobs/{id}` is idempotent and aborts active work
- [x] Add short-lived one-shot desktop intents for selected-text handoff
- [x] Let `run` own an embedded service at the stable `http://localhost:3846` desktop origin when no external service is healthy
  - Embedded instances never publish or delete the standalone discovery file

**Validation:** `cargo test --workspace`, `cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello"`, `cargo run -p codex-voice-app --bin codex-voice -- server`

---

## Phase 3 — Linux Desktop (Complete)

KDE6/Wayland portal-based hotkeys, text injection, and desktop UI surface.

- [x] Implement `LinuxHotkeyService` via XDG GlobalShortcuts portal (`ashpd`)
  - Binds `Control-M` + keyboard dictation key, plus `Super-F6` for speak selection
  - Emits `Pressed`/`Released` from activation/deactivation signals
- [x] Implement `LinuxTextInjector` via RemoteDesktop portal + clipboard
  - Sets clipboard, sends Ctrl+V through portal keyboard session, restores clipboard
  - Persists restore tokens for reuse across process restarts
- [x] Implement `LinuxPermissionService` with portal diagnostics
- [x] Implement the unified Tauri 2 tray/webview shell
  - Status updates, Start Test Recording, Speak text..., Open Settings, Open Logs, Run Diagnostics, Quit
  - Supersedes the historical GTK3, ksni, and iced implementations
- [x] Implement desktop notification HUD (`notify-send`) for focus-safe status
- [x] Implement settings/status webview window using the React PWA's settings-only route
- [x] Add `doctor linux-portals`, `doctor paste`
- [x] Wire `codex-voice run` with full Linux engine + tray + HUD

**Validation:** `cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals`, `cargo run -p codex-voice-app --bin codex-voice -- run`

---

## Phase 4 — Windows Desktop (Complete)

Windows has a full desktop surface with system tray, settings window, and engine wiring.

- [x] Unblock Windows workspace compilation
- [x] Implement `WindowsHotkeyService` with `GetAsyncKeyState` polling for `Control-M`
  - `@crates/codex-voice-platform/AGENTS.md`
- [x] Implement `WindowsTextInjector` with clipboard + `SendInput(Ctrl+V)`
  - Waits for Control release before paste to avoid hotkey contamination
- [x] Implement `WindowsPermissionService` stub
- [x] Wire `codex-voice run` for Windows with system tray
- [x] Add Windows system tray through the unified Tauri 2 shell
  - Status updates, Start Test Recording, Speak text..., Open Settings, Open Logs, Run Diagnostics, Quit
  - `@crates/codex-voice-app/src/tray.rs`
- [x] Add Windows settings/status webview window
- [ ] Add Windows desktop notification HUD (deferred — tray tooltip used for v1)
- [x] Validate compilation on Windows VM
  - `cargo check --workspace` passes on Windows
  - `cargo run --bin codex-voice -- doctor hotkey` works (times out as expected without key press)
- [ ] Validate hotkey + paste in a real interactive desktop session
  - `SendInput` may be blocked by UIPI from non-elevated processes
- [ ] Consider `WH_KEYBOARD_LL` low-level hook if `GetAsyncKeyState` polling is unreliable

**Validation:** `cargo check --workspace` on Windows, `cargo run -p codex-voice-app --bin codex-voice -- doctor hotkey`, `cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "test"`

---

## Phase 5 — macOS Implementation

**ON HOLD (2026-07-07): no macOS hardware is available.** The macOS code compiles only in theory — it is maintained mechanically alongside Linux/Windows changes but has not been compiled, linted, or tested on a real target since the cross-platform refactors landed. Do not treat macOS as shipped or verified until hardware is available and this hold is lifted.

macOS has a complete desktop surface with global hotkeys, Accessibility text injection, clipboard fallback, tray, and notifications.

- [x] Implement `MacOSHotkeyService` using `global-hotkey` crate
  - Registers Control-M globally via `GlobalHotKeyManager`
  - Emits `Pressed`/`Released` from `GlobalHotKeyEvent` receiver
  - `@crates/codex-voice-platform/src/macos.rs`
- [x] Implement `MacOSTextInjector`
  - Primary: Accessibility API (`AXUIElementSetAttributeValue` with `AXSelectedText`)
    - Raw FFI to `ApplicationServices` framework with manual CFString creation
  - Fallback: Clipboard + `CGEvent` Command-V paste
    - Raw FFI to `CoreGraphics` framework for `CGEventCreateKeyboardEvent`/`CGEventPost`
  - Pasteboard restoration via `arboard`
  - `@crates/codex-voice-platform/src/macos.rs`
- [x] Implement `MacOSPermissionService`
  - `AXIsProcessTrustedWithOptions` for Accessibility trust status
  - Opens System Settings via `open x-apple.systempreferences:...` for microphone and accessibility
- [ ] Add `NSMicrophoneUsageDescription` and background-app configuration
  - Requires `resources/macos/Info.plist` (packaging artifact, see Phase 7)
- [x] Wire `codex-voice run` for macOS with tray loop
  - Same event loop pattern as Linux/Windows: hotkey + app events + tray commands
  - `@crates/codex-voice-app/src/main.rs`
- [x] Add macOS diagnostics: `doctor hotkey`, `doctor paste`
  - `doctor hotkey` uses the same `global-hotkey` service
  - `doctor paste` tests Accessibility + clipboard fallback
- [x] Add macOS system tray through the unified Tauri 2 shell
  - Status updates, Start Test Recording, Speak text..., Open Settings, Open Logs, Run Diagnostics, Quit
- [x] Add macOS notification HUD (`osascript display notification`)
  - Best-effort desktop notifications with sound and replace semantics

**Validation:** Must test from a packaged `.app` for accurate Accessibility/microphone permission behavior; do not rely only on `target/debug` binary. Compilation verified on Linux host; macOS-only code paths are behind `#[cfg(target_os = "macos")]`.

---

## Phase 6 — Cross-Platform UI (Complete)

- [x] Consolidate all platforms on a Tauri 2 tray and webview shell in `codex-voice-app`
- [x] Remove the separate `codex-voice-ui` crate and its GTK3, ksni, and iced stacks
- [x] Keep status HUD delivery platform-native and focus-safe

---

## Phase 7 — Packaging & Distribution

- [x] Add canonical desktop icons and a Tauri 2 bundle manifest
- [x] Configure Windows NSIS current-user packaging with WebView2 bootstrapper download
- [x] Keep a portable Windows ZIP and emit SHA-256 files for both artifacts
- [x] Add Windows package and macOS compile checks to GitHub CI
- [ ] Configure signed/notarized macOS packages
- [ ] Configure Linux AppImage/deb/Pacman packages; `mise run setup` remains the Linux install path

**Validation:** `packaging/windows/build-dist.ps1`, plus the interactive checklist in `packaging/windows/SMOKE.md`.

---

## Phase 8 — End-to-End Validation

- [ ] Validate Linux KDE6/Wayland
  - App starts, tray appears, hotkey press/release works, transcription inserts, short recordings ignored, auth refresh works, logs identify insertion method
- [ ] Validate Windows 11
  - Packaged install launches without console (release mode), tray appears, hotkey + transcription + paste work into Notepad/Terminal/VS Code/Chromium
  - UIPI/elevation edge cases handled gracefully
- [ ] Validate macOS
  - Packaged `.app` launches as background/menu-bar utility, microphone permission prompt works, Accessibility status visible, hotkey + transcription + paste work into TextEdit/Terminal/VS Code/Safari
- [ ] Update `README.md` with final platform status and install instructions

---

## Deferred (Out of Scope)

These are explicitly not part of the current roadmap unless a future revision says otherwise:

- Official OpenAI API key backend (currently uses private Codex endpoint)
- Streaming Realtime transcription
- Signed/notarized macOS releases
- Windows code signing
- Auto-update mechanism
- App Store / Microsoft Store packaging
- Mobile support (iOS, Android)
- Full removal of the legacy Swift app (`CodexVoice/`)

---

## Architecture Reference

```
crates/codex-voice-app     → CLI, runtime wiring, `codex-voice` binary
crates/codex-voice-core    → State machine, traits, events, config
crates/codex-voice-audio   → CPAL capture, WAV writer
crates/codex-voice-codex   → Codex auth, private transcription HTTP
crates/codex-voice-tts     → Google Gemini + ElevenLabs TTS backends
crates/codex-voice-platform → Linux/Wayland portal adapters, Windows adapters
crates/codex-voice-transcriber → Local OpenAI-compatible audio service + embedded web PWA
crates/codex-voice-app     → CLI wiring plus the unified Tauri tray/webview desktop shell
web/                       → Standalone React TTS PWA (Vite/TypeScript/Tailwind), embedded into the transcriber crate at build time
```

For crate-specific conventions, build commands, and common gotchas, see each crate's `@AGENTS.md`.

For the original detailed research, milestone breakdown, and platform-specific API references, see `@docs/execplan-rust-native-cross-platform.md.ARCHIVED`.
