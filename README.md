# Codex Voice

Rust-native Linux-first implementation of the Codex Voice hold-to-dictate utility.

The current milestone keeps the old Swift app out of scope and builds the Linux
runtime first:

- Rust workspace with separate core, audio, Codex, platform, and app crates.
- CPAL microphone capture to mono 16-bit WAV.
- Core dictation state machine for press, release, transcribe, insert, and cleanup.
- Codex auth reuse through `~/.codex/auth.json` plus `codex app-server --listen stdio://`
  refresh.
- Private Codex transcription endpoint compatibility.
- Local OpenAI-compatible transcription service for tools such as `summarize`.
- Local OpenAI-compatible text-to-speech (TTS) service backed by Google Gemini TTS or ElevenLabs.
- Linux KDE/Wayland diagnostics for portal availability.
- Linux clipboard paste diagnostic using RemoteDesktop portal keyboard events.
- Cross-platform Tauri 2 desktop shell: a system tray plus plain webview windows
  that load the existing React PWA over HTTP (no bundler, no Tauri IPC — windows
  are external-URL webviews). System notification status HUD, log file,
  diagnostics, test recording, speak-text, settings, and quit menu actions.

## Commands

```bash
cargo run -p codex-voice-app --bin codex-voice -- --version
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 2
cargo run -p codex-voice-app --bin codex-voice -- doctor codex-auth
cargo run -p codex-voice-app --bin codex-voice -- doctor transcribe --file /path/to/sample.wav
cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "codex voice portal paste test"
cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello from codex voice"
cargo run -p codex-voice-app --bin codex-voice -- tts bench --dry-run
cargo run -p codex-voice-app --bin codex-voice -- server
cargo run -p codex-voice-app --bin codex-voice -- transcriber probe-limits --file /path/to/long-audio.wav
cargo run -p codex-voice-app --bin codex-voice -- run
```

The desktop `run` command requires a built frontend when no standalone server
is already available. On a clean checkout, run `mise run web-build` first (or
use `mise run dev` for the normal full-stack development loop).

`run` binds Control-M for hold-to-dictate and binds Super-F6 (Command-F6 on
macOS, Win-F6 on Windows) to speak the currently selected text — pressing it
opens (or focuses) the main window with the selection prefilled and speech
generation started automatically. It exposes a Tauri tray with a status label
plus `Start Test Recording`, `Speak text...` (opens the main window), `Open
Settings` (opens the settings window), `Open Logs`, `Run Diagnostics`, and
`Quit` actions. The main and settings windows are plain webviews that load the
same React PWA (`{base}/web?app=1` and `{base}/web?app=1&view=settings`); there
is no native settings/speak-text UI and no Tauri IPC. `run` reuses a discovered
service only when `/healthz` reports desktop readiness and its root is the
canonical `http://localhost:3846` origin; otherwise it self-hosts there. Keeping
one desktop origin preserves the PWA's localStorage and IndexedDB. The
embedded instance is owned by the desktop process and deliberately does not
publish a discovery file. Selected text is handed to the PWA through a
short-lived, one-shot desktop intent rather than being placed in the URL. All
TTS generation and playback for speak-text happens in the PWA via the
`/web/speech` endpoints described below.

## Local Audio Server

`server` exposes a localhost OpenAI-compatible audio endpoint:

```bash
cargo run -p codex-voice-app --bin codex-voice -- server
```

The service listens on `127.0.0.1:3845` by default, accepts
`POST /v1/audio/transcriptions`, `POST /audio/transcriptions`,
`POST /v1/audio/speech`, and `POST /audio/speech`, and writes a private discovery file to
`${XDG_STATE_HOME:-~/.local/state}/codex-voice/transcriber.json`. The discovery
file includes the service URL, OpenAI-compatible base URL, token, and PID. It is
written with mode `0600` on Unix.

To use it from `summarize` without patching `summarize`:

```bash
export OPENAI_WHISPER_BASE_URL="$(jq -r .openai_base_url ~/.local/state/codex-voice/transcriber.json)"
export OPENAI_API_KEY="$(jq -r .token ~/.local/state/codex-voice/transcriber.json)"
export SUMMARIZE_TRANSCRIBER=whisper
export SUMMARIZE_DISABLE_LOCAL_WHISPER_CPP=1
```

The service defaults to a 24 MiB Codex upload limit per backend request and a
512 MiB client upload limit. Oversized uploads are split with `ffmpeg` into
16 kHz mono WAV chunks before transcription. If `ffmpeg` is unavailable, the
service returns `413 Payload Too Large` with a clear error.

`transcriber probe-limits --file <audio>` tests the real Codex backend with a
source file or generated chunks and prints only sizes, status, transcript
lengths, and redacted errors.

## Web App

`server` also serves an installable TTS web app (PWA) from the same
listener, so any device reachable at the bind address — `localhost` for local
use, or the Tailscale/homelab address from `mise run setup` and the
Homelab Reverse-Proxy Setup above — can generate and play speech without a
native client.

The app is a standalone React frontend that lives at `web/` in the repo root
(Vite + React + TypeScript + Tailwind, built with `bun`). `bun run build` in
`web/` produces `web/dist`; the transcriber crate's `build.rs` copies that
directory into the build output and embeds it in the binary, so the app ships
inside `codex-voice`. Cargo builds never require `bun`: when `web/dist` is
absent the build embeds a stub page and prints a cargo warning. See
[`web/README.md`](web/README.md) for the stack, dev workflow, PWA/manifest
details, and the route-shadowing constraint.

The app lets you paste or type text and generate speech (including
generate-on-paste), scrub playback on a touch-friendly waveform, and install
itself to a phone or desktop homescreen via the web manifest and service
worker.

Routes: `GET /web/` (app shell), `GET /web/assets/*` (content-hashed,
immutable JS/CSS/icons), `GET /web/config` (browser-facing TTS config),
`GET /web/sw.js` (service worker), `POST /web/speech` (synchronous synthesis),
`POST /web/speech-jobs` and `GET /web/speech-jobs/{id}` (async speech jobs for
longer input), and `DELETE /web/speech-jobs/{id}` (idempotent cancellation).
Desktop selection handoff uses `POST /web/desktop-intents`, one-shot
`GET /web/desktop-intents/{id}`, and idempotent `DELETE` cleanup.
The service admits at most three nonterminal jobs and executes one synthesis
job at a time; overload returns `429` with `Retry-After`. The manifests (`manifest.webmanifest`,
`manifest-light.webmanifest`) and install icons are static files under
`web/public/`. Only `/web/assets/*` is served with immutable caching; the app
shell, service worker, manifests, and icons are served `no-cache`, and Workbox
content-hash revisions handle service-worker precache versioning. The legacy
`/web-sw.js` route now serves a small self-destructing worker so previously
installed PWAs unregister and adopt the new `/web/sw.js`; it can be removed
after a couple of releases.

### Serving a dist from disk

`server --web-dist <dir>` serves a built dist directory from disk instead of
the embedded copy (a full shadow, not a per-file fallback). This lets web
deploys ship independently of the Rust binary — rebuild `web/dist` and point
the running service at it without recompiling `codex-voice`.

### Development

Run `mise run dev` for the one-command full stack (audio server + Vite dev
server with HMR; Ctrl-C stops both), or run the two halves side by side:

```bash
# terminal 1 — backend (default 127.0.0.1:3845)
cargo run -p codex-voice-app --bin codex-voice -- server

# terminal 2 — frontend (Vite dev server on :5173, proxies /web/config and
# /web/speech* to the backend; override with CODEX_VOICE_BACKEND)
cd web && bun run dev
```

Then browse http://localhost:5173/web/. From the repo root, the mise tasks
`web-dev`, `web-install`, `web-build`, `web-check` (oxlint + oxfmt + tsc),
`web-test` (vitest), and `web-fmt` wrap the frontend toolchain, and `serve`
runs the backend alone; `mise run verify` includes `web-check` and `web-test`,
`mise run test-web` runs the Playwright suite against a freshly built frontend,
`mise run test-web-live` runs the paid single-run live TTS smoke, and
`mise run setup` builds the frontend before the release binary. The full
command table lives in the "Web Frontend" section of `AGENTS.md`.

`/web/config` and the `/web/speech*` routes are deliberately unauthenticated
so the PWA can call them without a bearer token. The config includes provider
keys and, for Codex prep, a refresh-capable OAuth bundle; the trust boundary is
private-network/Tailscale-only deployment, per the "Deployment Context"
section of the root `AGENTS.md`.
Browser requests carrying an Origin are accepted for `/web/config` only from
`https://voice.heliasar.com` or the Vite development origins on loopback port
`5173`; the broader CORS policy used by the OpenAI-compatible API cannot read
this credential payload. Codex auth is re-read from its configured file for
each config response so a server-side refresh is not exported as a stale
snapshot. Browser OAuth rotation is marked pending in origin storage and
atomically synchronized back to the configured auth file through
`POST /web/codex-auth` when the backend becomes reachable again; same-account,
non-older bundles are the only accepted updates.

## Text-to-Speech (TTS)

`server` includes TTS when `~/.codex/read-aloud-defaults.json` is present and
valid. If TTS config is absent, transcription still works and the speech endpoint
returns `503`. See `docs/read-aloud-defaults.example.json` for an example
config with a persona, both provider backends, and a `speechPrep` block
(placeholder API keys only — replace them with real values or env-sourced
secrets before use).

The TTS endpoint accepts standard OpenAI TTS JSON requests:

```json
{
  "model": "gpt-4o-mini-tts",
  "input": "Hello from Codex Voice.",
  "response_format": "wav",
  "speed": 1.0
}
```

Supported `response_format` values: `mp3`, `opus`, `aac`, `flac`, `wav`, `pcm`.
Provider-native output is converted to the requested format before returning.
Google Gemini TTS currently returns raw `audio/L16;codec=pcm;rate=24000`, so
`wav` is wrapped locally and compressed/container formats require `ffmpeg` on
`PATH`.

`voice` is optional. When omitted, the configured default persona/provider voice
is used. When present, it can be a configured persona name (e.g. `"sky"`) or a
provider-native voice identifier for the selected/default provider. If a persona
is configured, the service uses the persona's primary provider and preserves
persona context (scene, style, pace) across fallback to the other provider.

Google Gemini TTS voice names use star/planet identifiers such as `zephyr`,
`aoede`, `callirrhoe`, `charon`, `gacrux`, `orion`, and `puck`. OpenAI-style
names like `alloy`, `echo`, and `fable` are **not** supported by the Google
backend and will return `400 INVALID_ARGUMENT`.

`doctor tts` tests TTS config loading and optionally performs a live synthesis:

```bash
cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello world"
```

`tts bench` measures speech-prep (performance-tagging) latency and output for a
fixed sample across the default Codex and Google prep-model set, reusing the same
`SpeechPrepClient`/Codex clients the service uses. Use `--dry-run` to print the
planned requests without any network calls; `--models`, `--text`/`--file`, and
`--iterations` tune the run. It replaces the deprecated
`scripts/tts_prep_benchmark.py`.

```bash
cargo run -p codex-voice-app --bin codex-voice -- tts bench --dry-run
cargo run -p codex-voice-app --bin codex-voice -- tts bench --models gemini-3.5-flash
```

The `read-aloud-defaults.json` config is read from `~/.codex/read-aloud-defaults.json`
and supports Google Gemini TTS and ElevenLabs backends with persona-aware
provider fallback. Optional `messages.tts.speechPrep` uses a configured Codex or
Google generation model before synthesis. Codex prep defaults to `gpt-5.6-luna`
with no reasoning. Its default mode is `performance-tags`, which
preserves the original wording and inserts sparse inline bracketed audio tags
such as `[tender]`, `[sigh]`, or `[light chuckle]` only when the selected speech
model supports them. Set `"mode": "shorten"` explicitly for the older over-limit
shortening behavior. Model support is inferred for known tag-aware models and can
be overridden per provider with `"inlineAudioTags": true` or `false`. Set
`speechPrep.provider`, `speechPrep.model`, and `speechPrep.reasoningEffort` to
override the Codex defaults; Google prep models must support `generateContent`.

## User Service Setup

`mise run setup` builds the release binary, installs it to `/usr/local/bin/codex-voice`,
installs the wrapper script, installs the `com.heliasar.CodexVoice.desktop`
desktop entry used for portal identity, installs two systemd user units, reloads
systemd, enables both services, and starts the background server immediately:

- `codex-voice.service` runs `codex-voice run` after the graphical session is available; setup enables it but does not start it immediately.
- `codex-voice-server.service` runs `codex-voice server` as a background user service without bearer token authentication, so any OpenAI-compatible client on the same machine can use it.
- The Linux desktop process registers `com.heliasar.CodexVoice` with the host portal before opening GlobalShortcuts or RemoteDesktop sessions; keep that app id and the installed desktop filename in sync.

```bash
mise run setup
systemctl --user status codex-voice.service codex-voice-server.service
```

## Homelab Reverse-Proxy Setup

Saga proxies `voice.heliasar.com` across the Tailscale network to the service
host. This lets any Tailnet client reach the PWA and OpenAI-compatible endpoints
without knowing the backend machine's Tailscale IP. The legacy
`codex-voice.heliasar.com` hostname redirects to `voice.heliasar.com`.

### Architecture

```
Client ──https──▶ Saga (Caddy 80/443)
                    │
                    └──reverse_proxy──▶ asgard (Tailscale 100.120.202.119:3845)
```

- **Saga** is the ingress node; Tailnet-bound Caddy owns `80/443`.
- **asgard** runs the actual `codex-voice server` bound to `0.0.0.0:3845`.
- Caddy handles TLS termination with the wildcard `heliasar.com` certificate.
- Saga handles only exact `POST /_codex/responses` independently, forwarding
  it to ChatGPT so a warmed PWA can perform Codex prep while asgard is offline.

### Saga Caddy snippet

`/opt/homelab/config/caddy/Caddyfile`:

```caddy
voice.heliasar.com {
    tls /etc/saga-tls/heliasar.com.crt /etc/saga-tls/heliasar.com.key
    encode zstd gzip

    @codex_responses {
        method POST
        path /_codex/responses
    }
    handle @codex_responses {
        rewrite * /backend-api/codex/responses
        reverse_proxy https://chatgpt.com {
            header_up Host chatgpt.com
        }
    }
    handle /_codex/* {
        respond "Not Found" 404
    }
    handle {
        reverse_proxy 100.120.202.119:3845
    }
}
```

The scoped relay is not a general ChatGPT proxy: every other `/_codex/*` path
or method is rejected. It still requires the PWA's cached bearer token and
account ID.

### asgard systemd unit

`~/.config/systemd/user/codex-voice-server.service`:

```ini
[Unit]
Description=Codex Voice local OpenAI-compatible audio server

[Service]
Type=simple
ExecStart=/usr/local/bin/codex-voice-wrapper server --bind <tailscale-ip>:3845
Restart=on-failure
RestartSec=2
Environment=PATH=/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=default.target
```

Replace `<tailscale-ip>` with the host's explicit Tailscale address. Wildcard
binds are intentionally rejected; loopback and Tailscale addresses are the
supported server surfaces.

The wrapper script sources `~/personal/dotfiles/secrets.sh` (API keys, TTS config)
before launching the binary.

### Verification

```bash
# From any Tailnet host
curl -s https://voice.heliasar.com/healthz
# Expected: {"ok":true,"capabilities":{"transcriptions":true,"speech":true}}
```

If asgard is offline, ordinary API routes return a gateway error while a warmed,
service-worker-controlled PWA can load its cached shell/config and fall back to
direct provider generation. Codex prep uses Saga's `/_codex/responses` relay;
OAuth refresh goes directly to `auth.openai.com` and does not require the Codex
CLI.

## Linux Notes

On KDE/Wayland, verify the desktop first:

```bash
echo "$XDG_SESSION_TYPE"
echo "$XDG_CURRENT_DESKTOP"
```

`doctor linux-portals` checks the GlobalShortcuts and RemoteDesktop portal
interfaces through the user D-Bus. `doctor paste` sets the clipboard and sends
Ctrl+V through a RemoteDesktop keyboard portal session. The first run may ask for
desktop portal approval; subsequent runs reuse the persisted restore token when
the portal returns one.

The Linux desktop surface is the same Tauri tray and webview windows used on
all platforms (tray-icon + appindicator for the tray, webkit2gtk for the
windows). It mirrors dictation state in the tray and uses `notify-send` for
the HUD so it does not steal focus from the target app. Logs are written to
`${XDG_STATE_HOME:-~/.local/state}/codex-voice/codex-voice.log`, and the tray
provides menu actions for test recording, speak-text, settings, logs, portal
diagnostics, and quitting the background app.

## System requirements

Linux runtime and build dependencies for the Tauri tray/webview shell:

- Arch/CachyOS: `webkit2gtk-4.1 libappindicator-gtk3 librsvg`
- Debian/Ubuntu: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev`

The Windows NSIS installer uses Tauri's WebView2 bootstrapper download mode, so
it installs the Evergreen runtime when the target machine does not already have
it. Build both the installer and portable ZIP with
`packaging/windows/build-dist.ps1`; SHA-256 files are emitted beside them.
macOS uses the system WKWebView and needs no extra dependencies.

## Validation

```bash
cargo fmt --check
cargo check --workspace
cargo test --workspace
```
