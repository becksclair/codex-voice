# Codex Voice — Roadmap

Rust-native, cross-platform hold-to-dictate desktop utility. Press and hold `Control-M`, speak, release, and the transcript is inserted into the focused application.

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
- [x] Add `transcriber probe-limits` for backend stress testing
- [x] Add `mise run setup` for Linux systemd user service install

**Validation:** `cargo test --workspace`, `cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello"`, `cargo run -p codex-voice-app --bin codex-voice -- server`

---

## Phase 3 — Linux Desktop (Complete)

KDE6/Wayland portal-based hotkeys, text injection, and desktop UI surface.

- [x] Implement `LinuxHotkeyService` via XDG GlobalShortcuts portal (`ashpd`)
  - Binds `Control-M` + keyboard dictation key
  - Emits `Pressed`/`Released` from activation/deactivation signals
- [x] Implement `LinuxTextInjector` via RemoteDesktop portal + clipboard
  - Sets clipboard, sends Ctrl+V through portal keyboard session, restores clipboard
  - Persists restore tokens for reuse across process restarts
- [x] Implement `LinuxPermissionService` with portal diagnostics
- [x] Implement GTK tray with `tray-icon`
  - Status updates, Start Test Recording, Open Settings, Open Logs, Run Diagnostics, Quit
- [x] Implement desktop notification HUD (`notify-send`) for focus-safe status
- [x] Implement GTK settings/status window
- [x] Add `doctor linux-portals`, `doctor paste`
- [x] Wire `codex-voice run` with full Linux engine + tray + HUD

**Validation:** `cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals`, `cargo run -p codex-voice-app --bin codex-voice -- run`

---

## Phase 4 — Windows Foundation (Partial)

Windows compiles and has basic engine + adapters, but no desktop UI surface.

- [x] Unblock Windows workspace compilation
- [x] Implement `WindowsHotkeyService` with `GetAsyncKeyState` polling for `Control-M`
  - `@crates/codex-voice-platform/AGENTS.md`
- [x] Implement `WindowsTextInjector` with clipboard + `SendInput(Ctrl+V)`
  - Waits for Control release before paste to avoid hotkey contamination
- [x] Implement `WindowsPermissionService` stub
- [x] Wire `codex-voice run` for Windows (console-only, no tray)
- [ ] Add Windows system tray (tray-icon supports Windows)
  - `@crates/codex-voice-ui/AGENTS.md`
- [ ] Add Windows desktop notification HUD
- [ ] Add Windows settings/status window
- [ ] Validate hotkey + paste in a real interactive desktop session
  - `SendInput` may be blocked by UIPI from non-elevated processes
- [ ] Consider `WH_KEYBOARD_LL` low-level hook if `GetAsyncKeyState` polling is unreliable

**Validation:** `cargo check --workspace` on Windows, `cargo run -p codex-voice-app --bin codex-voice -- doctor hotkey`, `cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "test"`

---

## Phase 5 — macOS Implementation

No macOS code exists yet. The `run()` command returns `anyhow::bail!("this milestone implements Linux and Windows only")`.

- [ ] Implement `MacOSHotkeyService`
  - Option A: `global-hotkey` crate (supports macOS, press/release semantics)
  - Option B: Direct Carbon hotkey events if `global-hotkey` lacks fidelity
  - Research: `@docs/execplan-rust-native-cross-platform.md` (Slint/macOS research notes)
- [ ] Implement `MacOSTextInjector`
  - Primary: Accessibility selected-text replacement
  - Fallback: Clipboard + CGEvent Command-V, with pasteboard restoration
- [ ] Implement `MacOSPermissionService`
  - Microphone usage prompt, Accessibility trust checks, settings links
- [ ] Add `NSMicrophoneUsageDescription` and background-app configuration
- [ ] Wire `codex-voice run` for macOS
- [ ] Add macOS diagnostics: `doctor hotkey`, `doctor paste`

**Validation:** Must test from a packaged `.app` for accurate Accessibility/microphone permission behavior; do not rely only on `target/debug` binary.

---

## Phase 6 — Cross-Platform UI Decision

The Linux UI currently uses GTK + `tray-icon` + `notify-send`. Slint was planned in the original ExecPlan but was never adopted.

- [ ] **Decision needed:** Keep per-platform native UI or migrate to Slint
  - **Native pros:** Uses OS-native widgets, less binary size, no extra dependency
  - **Slint pros:** Single UI codebase for all platforms, Rust-native, no webview
  - Research: `@docs/execplan-rust-native-cross-platform.md` (Slint desktop docs reference)
- [ ] If keeping native: add Windows tray/HUD/settings (see Phase 4)
- [ ] If keeping native: add macOS tray/HUD/settings (see Phase 5)
- [ ] If migrating to Slint: implement Slint UI crate, replace Linux GTK surfaces, add Windows/macOS surfaces
  - Requires `slint` and `slint-build` dependencies
  - Research: [Slint desktop docs](https://docs.slint.dev/latest/docs/slint/guide/platforms/desktop/)

---

## Phase 7 — Packaging & Distribution

No packaging exists. No `cargo-packager` config, no `resources/`, no icons.

- [ ] Add `cargo-packager` to workspace dev-dependencies
  - Research: [cargo-packager docs](https://docs.rs/cargo-packager)
- [ ] Create `resources/icons/` with platform-specific icon sets
- [ ] Create `resources/macos/Info.plist` with `NSMicrophoneUsageDescription`
- [ ] Add `[package.metadata.packager]` to `crates/codex-voice-app/Cargo.toml`
  - Product name: `Codex Voice`
  - Identifier: `dev.codexvoice.app`
  - Category: `Productivity`
- [ ] Configure macOS packaging: `.app` + `.dmg`, background app, unsigned
- [ ] Configure Linux packaging: AppImage, `.deb`, Pacman
  - Document GTK/appindicator/portal package dependencies
- [ ] Configure Windows packaging: NSIS `.exe` (WiX `.msi` optional)
- [ ] Add `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` for Windows release builds
- [ ] Add packaging commands to `README.md`
- [ ] Keep `mise run setup` as the Linux developer install path

**Validation:** `cargo packager --release --format app,dmg` (macOS), `cargo packager --release --format appimage,deb,pacman` (Linux), `cargo packager --release --format nsis` (Windows)

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
crates/codex-voice-transcriber → Local OpenAI-compatible audio service
crates/codex-voice-ui      → Linux GTK tray, notifications, settings window
```

For crate-specific conventions, build commands, and common gotchas, see each crate's `@AGENTS.md`.

For the original detailed research, milestone breakdown, and platform-specific API references, see `@docs/execplan-rust-native-cross-platform.md.ARCHIVED`.
