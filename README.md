# Codex Voice

Rust-native Linux-first implementation of the Codex Voice hold-to-dictate utility.

The current milestone keeps the old Swift app out of scope and builds the Linux
runtime first:

- Rust workspace with separate core, audio, Codex, platform, UI, and app crates.
- CPAL microphone capture to mono 16-bit WAV.
- Core dictation state machine for press, release, transcribe, insert, and cleanup.
- Codex auth reuse through `~/.codex/auth.json` plus `codex app-server --listen stdio://`
  refresh.
- Private Codex transcription endpoint compatibility.
- Local OpenAI-compatible transcription service for tools such as `summarize`.
- Local OpenAI-compatible text-to-speech (TTS) service backed by Google Gemini TTS or ElevenLabs.
- Linux KDE/Wayland diagnostics for portal availability.
- Linux clipboard paste diagnostic using RemoteDesktop portal keyboard events.
- Desktop tray, system notification status HUD, settings/status window, log file,
  diagnostics, test recording, speak-text, and quit menu actions.

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

`run` binds Control-M for hold-to-dictate and binds Super-F6 (Command-F6 on
macOS, Win-F6 on Windows) to speak the currently selected text. It exposes a
desktop tray surface with status, settings, diagnostics, test recording,
`Speak text...`, replay, logs, and quit actions. If a healthy local transcriber
service is running, `run` uses it as the transcription backend; otherwise it
falls back to direct Codex transcription. Speech playback always uses the local
audio service's `/v1/audio/speech` endpoint.

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
longer input). The manifests (`manifest.webmanifest`,
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

Run the backend and the Vite dev server side by side:

```bash
# terminal 1 — backend (default 127.0.0.1:3845)
cargo run -p codex-voice-app --bin codex-voice -- server

# terminal 2 — frontend (Vite dev server on :5173, proxies /web/config and
# /web/speech* to the backend; override with CODEX_VOICE_BACKEND)
cd web && bun run dev
```

Then browse http://localhost:5173/web/. From the repo root, the mise tasks
`web-install`, `web-build`, `web-check` (oxlint + oxfmt + tsc), and `web-test`
(vitest) wrap the frontend toolchain; `mise run verify` includes `web-check`
and `web-test`, `mise run test-web` runs the Playwright suite against a freshly
built frontend, and `mise run setup` builds the frontend before the release
binary.

`/web/config` and the `/web/speech*` routes are deliberately unauthenticated
so the PWA can call them without a bearer token; the trust boundary is
private-network/Tailscale-only deployment, per the "Deployment Context"
section of the root `AGENTS.md`.

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
provider fallback. Optional `messages.tts.speechPrep` uses a configured Google
generation model before synthesis. Its default mode is `performance-tags`, which
preserves the original wording and inserts sparse inline bracketed audio tags
such as `[tender]`, `[sigh]`, or `[light chuckle]` only when the selected speech
model supports them. Set `"mode": "shorten"` explicitly for the older over-limit
shortening behavior. Model support is inferred for known tag-aware models and can
be overridden per provider with `"inlineAudioTags": true` or `false`. Use a
Google text generation model that supports `generateContent` for
`speechPrep.model`; `google/gemini-3.5-flash` is the expected current model name.

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

When the service runs on a different machine from the homelab ingress point, Orion
(Raspberry Pi) proxies `codex-voice.heliasar.com` across the Tailscale network to
the service host. This lets any Tailnet client reach the OpenAI-compatible endpoints
without knowing the backend machine's Tailscale IP.

### Architecture

```
Client ──https──▶ Orion (Caddy 80/443)
                    │
                    └──reverse_proxy──▶ asgard (Tailscale 100.120.202.119:3845)
```

- **Orion** is the ingress node; host Caddy owns `80/443`.
- **asgard** runs the actual `codex-voice server` bound to `0.0.0.0:3845`.
- Caddy handles TLS termination with the wildcard `heliasar.com` certificate.

### Orion Caddy snippet

`/opt/homelab/configs/caddy/generated/30-exact-sites.caddy`:

```caddy
codex-voice.heliasar.com {
    tls /etc/certs/heliasar.com.crt /etc/certs/heliasar.com.key
    encode zstd gzip
    reverse_proxy 100.120.202.119:3845 {
        header_up X-Forwarded-Proto https
        header_up Host {host}
    }
}
```

This is generated from the homelab `services.json` manifest, which marks the service
as `runtime.type: external` on machine `asgard` with a managed DNS alias.

### asgard systemd unit

`~/.config/systemd/user/codex-voice-server.service`:

```ini
[Unit]
Description=Codex Voice local OpenAI-compatible audio server

[Service]
Type=simple
ExecStart=/usr/local/bin/codex-voice-wrapper server --bind 0.0.0.0:3845
Restart=on-failure
RestartSec=2
Environment=PATH=/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=default.target
```

The wrapper script sources `~/personal/dotfiles/secrets.sh` (API keys, TTS config)
before launching the binary.

### Verification

```bash
# From any Tailnet host
curl -s https://codex-voice.heliasar.com/healthz
# Expected: {"ok":true,"capabilities":{"transcriptions":true,"speech":true}}
```

If asgard is offline, Caddy returns a gateway error; the manifest marks the service
`availability: optional` because it depends on the laptop being awake and on the
Tailnet.

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

The Linux desktop surface depends on GTK 3 plus AppIndicator/Ayatana
AppIndicator runtime libraries. It mirrors dictation state in the tray and uses
the desktop notification service for the HUD so it does not steal focus from the
target app. Logs are written to
`${XDG_STATE_HOME:-~/.local/state}/codex-voice/codex-voice.log`, and the tray
provides menu actions for test recording, settings/status, logs, portal
diagnostics, and quitting the background app.

## Validation

```bash
cargo fmt --check
cargo check --workspace
cargo test --workspace
```
