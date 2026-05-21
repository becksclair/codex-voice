# codex-voice-tts

## Package Identity

`codex-voice-tts` implements text-to-speech synthesis with Google Gemini TTS and ElevenLabs backends. It loads config from `~/.codex/read-aloud-defaults.json`, resolves personas, orchestrates provider fallback, and converts provider-native output to the requested format.

## Setup & Run

```bash
cargo check -p codex-voice-tts
cargo test -p codex-voice-tts
cargo clippy -p codex-voice-tts --all-targets -- -D warnings
cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "hello world"
```

Live Google TTS validation (requires `GEMINI_API_KEY` or `GOOGLE_API_KEY`):

```bash
export CODEX_VOICE_TTS_LIVE=1
cargo test -p codex-voice-tts google_live_synthesize -- --ignored
```

## Patterns & Conventions

- Config loading and persona resolution: `src/config.rs` (`ReadAloudConfigLoader`, `ResolvedTtsConfig`).
- Orchestration and fallback: `src/client.rs` (`ConfiguredSpeechClient`).
- Google Gemini client: `src/google.rs` (`GoogleSpeechClient`).
- ElevenLabs client: `src/elevenlabs.rs` (`ElevenLabsSpeechClient`).
- Format conversion: `src/convert.rs` (`convert_speech`), wraps PCM as WAV in-process; compressed/container outputs require `ffmpeg` on `PATH`.
- Secret resolution: `src/secret.rs` (`resolve_secret`).
- TTS input sanitization: `src/sanitize.rs` (`sanitize_for_tts`).
- ✅ DO: Implement the core `SpeechClient` trait from `codex-voice-core`.
- ✅ DO: Load config only on `server` startup and `doctor tts`, not on ordinary `codex-voice run`.
- ✅ DO: Keep TTS additive — a missing or broken config must not break dictation.
- ✅ DO: Return `503` from the speech endpoint when TTS is not configured.
- ✅ DO: Preserve persona context (scene, style, pace) across fallback to the other provider.
- ✅ DO: Sanitize input text before sending to providers; truncate at `max_text_length`.
- ❌ DON'T: Log API keys or full synthesized text; print byte size and content type only.
- ❌ DON'T: Move official OpenAI TTS API support here without a separate client behind the core trait.

## Touch Points / Key Files

- Orchestrator: `src/client.rs`
- Config loader: `src/config.rs`
- Google client: `src/google.rs`
- ElevenLabs client: `src/elevenlabs.rs`
- Format conversion: `src/convert.rs`
- Core speech trait: `crates/codex-voice-core/src/speech.rs`
- CLI diagnostic: `crates/codex-voice-app/src/tts.rs`
- Service wiring: `crates/codex-voice-app/src/transcriber.rs`

## JIT Index Hints

```bash
rg -n "ConfiguredSpeechClient|SpeechClient|synthesize" src/client.rs
rg -n "ReadAloudConfigLoader|ResolvedTtsConfig|ProviderKind|persona" src/config.rs
rg -n "GoogleSpeechClient|generateContent|responseModalities" src/google.rs
rg -n "ElevenLabsSpeechClient|/v1/text-to-speech" src/elevenlabs.rs
rg -n "convert_speech|pcm_to_wav|ffmpeg" src/convert.rs
rg -n "sanitize_for_tts|max_text_length" src/sanitize.rs
rg -n "doctor_tts|TtsDoctor" ../codex-voice-app/src/tts.rs
```

## Common Gotchas

- Google Gemini TTS returns raw PCM (`audio/L16;codec=pcm;rate=24000`), not WAV. `convert_speech` wraps it when the request asks for WAV.
- `ffmpeg` is required on `PATH` for compressed/container outputs (MP3, Opus, AAC, FLAC) from PCM.
- ElevenLabs is implemented but currently unvalidated in CI because the account has no credits. Google is the first proven lane.
- The config file `~/.codex/read-aloud-defaults.json` supports persona-aware provider fallback. If a persona is configured, the service uses the persona's primary provider and preserves persona context on fallback.
- Live tests are gated behind `CODEX_VOICE_TTS_LIVE=1` to avoid burning API keys in normal `cargo test` runs.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-tts && cargo test -p codex-voice-tts && cargo clippy -p codex-voice-tts --all-targets -- -D warnings
```
