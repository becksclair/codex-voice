# Codex Voice Agent Guide

## Project Snapshot

Codex Voice is a Rust workspace for a Linux-first, Rust-native hold-to-dictate desktop utility. The workspace is split into small crates for app wiring, core state, audio capture, Codex auth/transcription, TTS, and platform adapters. The desktop UI is a Tauri 2 shell (tray + webview windows) that lives inside `codex-voice-app`; there is no separate UI crate. Read the nearest crate-level `AGENTS.md` before editing files under `crates/**`.

## Root Setup Commands

```bash
cargo fetch
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p codex-voice-app --bin codex-voice -- --version
```

Linux smoke checks:

```bash
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
timeout 10s cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 1
cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello"
```

## Universal Conventions

- Use Rust 2021 and workspace-managed dependency versions from `Cargo.toml`.
- Keep crates small and boundary-focused; shared contracts live in `crates/codex-voice-core`.
- Prefer typed errors in library crates and `anyhow::Result` only at app/CLI boundaries.
- Keep generated/runtime artifacts out of git; `target/` is ignored.
- For PWA/web assets, never reference immutable assets through bare stable URLs. This is now enforced structurally: only content-hashed `/web/assets/*` paths are served with immutable caching, and the app shell, service worker, manifests, and icons are served `no-cache`. Workbox content-hash revisions handle service-worker precache versioning, so keep new long-lived assets under the hashed `assets/` directory rather than reintroducing a build-revision query string.
- Update `README.md` and `ROADMAP.md` when command contracts change.
- Preserve Linux-first scope until portal hotkey/paste proof is complete.

## Web Frontend (`web/`)

The TTS web PWA is a standalone React app at `web/`, decoupled from the Rust service. Deep reference: `web/README.md` (stack decisions, PWA/manifest details, route-shadowing constraint) and `web/PERFORMANCE.md` (bundle budget rationale, measured numbers).

### Architecture in one paragraph

`bun run build` in `web/` produces `web/dist` (content-hashed assets under `dist/assets/`). The transcriber crate's `build.rs` copies `web/dist` into `$OUT_DIR` and embeds it via `include_dir!`, so the release binary is self-contained. When `web/dist` is absent, a stub page is embedded instead and cargo prints a warning — plain cargo builds never require bun, and dist-content tests skip themselves. At runtime, `codex-voice server --web-dist <dir>` serves a dist directory from disk (fully shadowing the embedded copy), which allows deploying web updates without rebuilding the Rust binary. The JSON API surface (`/web/config`, `/web/speech`, `/web/speech-jobs*`) is served by Rust and is independent of the asset pipeline.

### Desktop app URL contract

`codex-voice run` opens Tauri webview windows pointed at the same PWA over
HTTP — there is no Tauri IPC, so the desktop integration is entirely
query-string/hash driven:

- `?app=1` — desktop app mode; the PWA skips service-worker registration.
- `?view=settings` — the settings drawer starts open.
- `#intent=<128-bit hex id>` — consumes a short-lived, one-shot selected-text
  intent from the local service and starts speech generation automatically.
  Used by the Super-F6 speak-selection hotkey without exposing text in the URL.

The main window loads `{base}/web?app=1`; the settings window loads
`{base}/web?app=1&view=settings`. Any code parsing these parameters or the
`#intent=` hash lives in `web/src/`.

### Using it

| Command | What it does |
| --- | --- |
| `mise run dev` | Full-stack development: audio server + Vite dev server with HMR; Ctrl-C stops both |
| `mise run web-dev` | Vite dev server only (HMR at `http://localhost:5173/web/`; proxies `/web` APIs to `CODEX_VOICE_BACKEND`, default `http://127.0.0.1:3845`) |
| `mise run serve` | Audio server only, foreground, default bind `127.0.0.1:3845` |
| `mise run web-build` | Production build into `web/dist` |
| `mise run web-check` | oxlint + oxfmt `--check` + `tsc --noEmit` |
| `mise run web-fmt` | Format in place with oxfmt |
| `mise run web-test` | vitest unit/component suite |
| `mise run test-web` | Playwright e2e suite (builds the frontend first, spawns the real Rust server) |
| `mise run test-web-live` | Single-run live TTS smoke — **paid** (~2k characters); needs real `~/.codex/read-aloud-defaults.json`; ElevenLabs leg opt-in via `LIVE_TTS_ELEVENLABS=1` |
| `mise run setup` | Builds the frontend, then the release binary (embedding it), installs, and enables user services |

Typical development loop: `mise run dev`, edit under `web/src/`, changes hot-reload instantly; the Rust server only needs a restart when Rust code changes. To point HMR at the deployed Tailscale instance instead of a local server: `CODEX_VOICE_BACKEND=http://<tailscale-ip>:3845 mise run web-dev`.

### Developing it

- Package manager is `bun` — never `npm` or `npx`; use `bunx` for one-off executables. Lint/format with oxlint/oxfmt, unit-test with vitest, TypeScript stays strict.
- Layout: pure logic in `web/src/lib/` (storage, config, settings, theme, synthesis, prep, streaming, generation — DOM-free and unit-tested), hooks in `web/src/hooks/`, components in `web/src/components/`. Keep new logic in `lib/` with tests; keep components thin.
- The React Compiler is enabled: do not add `useMemo`/`useCallback`/`memo` for performance — write plain code and let the compiler memoize.
- Styling is Tailwind 4 utilities plus a small themed-token layer in `web/src/index.css`. Theming works via `data-theme` on `<html>` (set pre-paint by an inline script in `index.html`). The CSS custom properties read by JS at runtime (e.g. `--waveform-*`) are load-bearing names — do not rename.
- **Frozen test contracts** (Playwright and component tests assert on these; do not rename): the 23 element IDs (`#text`, `#generate`, `#count`, `#paste`, `#settings-toggle`, `#generate-on-paste`, `#theme`, `#waveform`, …), the localStorage keys `codex-voice.web.{text,config.v1,settings.v1,generation.v1}`, and the IndexedDB names `codex-voice-web-audio`/`generated`/`last`.
- An initial-load JS budget (80 kB gzip) is enforced by `bun run build`; if a change trips it, prefer moving code behind the generation-boundary dynamic import (see `web/PERFORMANCE.md`) over raising the budget.
- Cache policy is structural: only content-hashed `/web/assets/*` paths are immutable; keep long-lived assets under the hashed `assets/` directory.

## Security & Secrets

- Never print or commit access tokens, refresh tokens, full account IDs, full transcripts, or private audio.
- Codex auth is read from `~/.codex/auth.json`; do not write this file directly.
- TTS secrets are read from `~/.codex/read-aloud-defaults.json`; never log API keys.
- Diagnostics may print token presence, redacted account IDs, transcript length, and short previews only.
- Temp WAV files must be deleted unless the user explicitly asks to keep a diagnostic recording.

## Deployment Context

- This project is private and local-only, accessed behind Tailscale. The local transcriber service bearer token is for basic request identification within the trusted network, not for strong authentication.

## JIT Index

### Package Structure

- App/CLI wiring: `crates/codex-voice-app/` -> [see AGENTS.md](crates/codex-voice-app/AGENTS.md)
- Core traits/state machine: `crates/codex-voice-core/` -> [see AGENTS.md](crates/codex-voice-core/AGENTS.md)
- Audio capture: `crates/codex-voice-audio/` -> [see AGENTS.md](crates/codex-voice-audio/AGENTS.md)
- Codex auth/transcription: `crates/codex-voice-codex/` -> [see AGENTS.md](crates/codex-voice-codex/AGENTS.md)
- TTS (Google Gemini + ElevenLabs): `crates/codex-voice-tts/` -> [see AGENTS.md](crates/codex-voice-tts/AGENTS.md)
- Transcriber service/client/discovery: `crates/codex-voice-transcriber/` -> [see AGENTS.md](crates/codex-voice-transcriber/AGENTS.md)
- Platform adapters: `crates/codex-voice-platform/` -> [see AGENTS.md](crates/codex-voice-platform/AGENTS.md)
- Tauri tray/window shell (no separate UI crate): `crates/codex-voice-app/src/tray.rs`, `src/status.rs`, `src/hud.rs` -> [see AGENTS.md](crates/codex-voice-app/AGENTS.md)
- Web frontend (React PWA): `web/` -> [see README.md](web/README.md); e2e suite: `webtests/` -> [see README.md](webtests/README.md)
- Architecture plan: `ROADMAP.md` (see `docs/execplan-rust-native-cross-platform.md.ARCHIVED` for original detailed research)

### Quick Find Commands

```bash
rg -n "DictationEngine|HotkeyEvent|TextInjector|AudioRecorder|TranscriptionClient" crates
rg -n "doctor|Parser|Subcommand" crates/codex-voice-app/src
rg -n "cpal|WavWriter|RecordedAudio" crates/codex-voice-audio/src
rg -n "auth|transcribe|TRANSCRIBE_URL|account/read" crates/codex-voice-codex/src
rg -n "tts|speech|synthesize|ProviderKind|FallbackPolicy|ReadAloudConfig" crates/codex-voice-tts/src
rg -n "GlobalShortcuts|RemoteDesktop|Clipboard|PortalTokenStore|PortalPaste" crates/codex-voice-platform/src
find crates -name '*test*' -o -name '*.rs'
```

## Definition of Done

- Relevant crate-level checks pass, then root `cargo fmt --check`, `cargo check --workspace`, and `cargo test --workspace`.
- Run `cargo clippy --workspace --all-targets -- -D warnings` after non-trivial Rust changes.
- After web frontend changes, run the web gates (`mise run web-check` and `mise run web-test`); `mise run verify` runs the full Rust + web gate set. For behavioral or DOM changes, also run `mise run test-web` (Playwright).
- After modifying any installed/runtime service behavior, rebuild and restart the affected user services before marking the task complete, then verify the restarted service is healthy.
- For Linux runtime changes, run `doctor linux-portals` and the 1-second `doctor audio` smoke when relevant.
- For TTS changes, run `doctor tts` with a short test phrase.
- Update README/ROADMAP for changed CLI, runtime, auth, platform, or TTS behavior.
