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
- Linux KDE/Wayland diagnostics for portal availability.
- Linux clipboard paste diagnostic using RemoteDesktop portal keyboard events.

## Commands

```bash
cargo run -p codex-voice-app --bin codex-voice -- --version
cargo run -p codex-voice-app --bin codex-voice -- doctor linux-portals
cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 2
cargo run -p codex-voice-app --bin codex-voice -- doctor codex-auth
cargo run -p codex-voice-app --bin codex-voice -- doctor transcribe --file /path/to/sample.wav
cargo run -p codex-voice-app --bin codex-voice -- doctor paste --text "codex voice portal paste test"
cargo run -p codex-voice-app --bin codex-voice -- run
```

`run` currently uses the Linux engine wiring and binds Control-M through the KDE
GlobalShortcuts portal for hold-to-dictate.

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

## Validation

```bash
cargo fmt --check
cargo check --workspace
cargo test --workspace
```
