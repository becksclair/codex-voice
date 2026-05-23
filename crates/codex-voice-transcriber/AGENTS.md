# Codex Voice Transcriber Agent Guide

## Package Identity

`codex-voice-transcriber` implements the local OpenAI-compatible transcription/speech service, service discovery, and the runtime client that probes it. It is the home for:

- `serve()` — the Axum HTTP service
- `RuntimeTranscriptionClient` — the app-side client with Codex fallback
- Service discovery file I/O

## Setup & Run

```bash
cargo check -p codex-voice-transcriber
cargo test -p codex-voice-transcriber
cargo clippy -p codex-voice-transcriber --all-targets -- -D warnings
```

## Package Structure

- `src/server.rs` — Axum routes, auth, transcription upload handler, speech endpoint
- `src/client.rs` — `LocalTranscriberClient` that probes `/healthz` and transcribes via the local service
- `src/discovery.rs` — discovery file read/write, token resolution, stale-PID cleanup
- `src/chunking.rs` — ffmpeg-based audio splitting for oversized uploads
- `src/upload.rs` — multipart upload parsing and temp file handling
- `src/lib.rs` — `serve()`, `RuntimeTranscriptionClient`, `resolve_transcription_backend()`, `probe_limits()`
- `src/test_support.rs` — fake backends and request builders for unit tests

## Patterns & Conventions

- The service exposes OpenAI-compatible endpoints: `POST /v1/audio/transcriptions`, `POST /v1/audio/speech`, `GET /v1/healthz`
- Non-v1 route aliases exist for convenience: `/audio/transcriptions`, `/audio/speech`, `/healthz`
- Authentication is optional by default; use `--require-auth` to enforce bearer tokens
- The `speech` endpoint returns `503` when TTS is not configured
- Oversized uploads are chunked with ffmpeg; without ffmpeg, return `413`
- The discovery file lives at `${XDG_STATE_HOME:-~/.local/state}/codex-voice/transcriber.json`
- `resolve_transcription_backend()` probes the local service first, then falls back to direct Codex

## Touch Points / Key Files

- Service router: `src/server.rs`
- Local client: `src/client.rs`
- Discovery: `src/discovery.rs`
- Chunking: `src/chunking.rs`
- Runtime resolution: `src/lib.rs`
- App wiring: `../codex-voice-app/src/main.rs`

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-transcriber && cargo test -p codex-voice-transcriber && cargo clippy -p codex-voice-transcriber --all-targets -- -D warnings
```
