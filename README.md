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
- Linux tray, system notification status HUD, settings/status window, log file,
  diagnostics, test recording, and quit menu actions.

## Commands

```bash
cargo run -p codex-voice-app --bin codex-voice -- --version
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 2
cargo run -p codex-voice-app --bin codex-voice -- doctor codex-auth
cargo run -p codex-voice-app --bin codex-voice -- doctor transcribe --file /path/to/sample.wav
cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "codex voice portal paste test"
cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello from codex voice"
cargo run -p codex-voice-app --bin codex-voice -- server
cargo run -p codex-voice-app --bin codex-voice -- transcriber probe-limits --file /path/to/long-audio.wav
cargo run -p codex-voice-app --bin codex-voice -- run
```

`run` currently uses the Linux engine wiring, binds Control-M plus the keyboard
dictation key through the KDE GlobalShortcuts portal for hold-to-dictate, and
exposes a Linux desktop surface with a tray menu, system notification status
HUD, and settings/status window. If a healthy local transcriber service is
running, `run` uses it as the transcription backend; otherwise it falls back to
direct Codex transcription.

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

## Text-to-Speech (TTS)

`server` includes TTS when `~/.codex/read-aloud-defaults.json` is present and
valid. If TTS config is absent, transcription still works and the speech endpoint
returns `503`.

The TTS endpoint accepts standard OpenAI TTS JSON requests:

```json
{
  "model": "gpt-4o-mini-tts",
  "voice": "sky",
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

`voice` is required. It can be a configured persona name (e.g. `"sky"`) or a
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

The `read-aloud-defaults.json` config is read from `~/.codex/read-aloud-defaults.json`
and supports Google Gemini TTS and ElevenLabs backends with persona-aware
provider fallback.

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
