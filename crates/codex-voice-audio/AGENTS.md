# codex-voice-audio

## Package Identity

`codex-voice-audio` implements microphone capture through CPAL and writes temporary mono 16-bit WAV files using `hound`. It implements the core `AudioRecorder` trait.

## Setup & Run

```bash
cargo check -p codex-voice-audio
cargo test -p codex-voice-audio
cargo clippy -p codex-voice-audio --all-targets -- -D warnings
timeout 10s cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 1
```

## Patterns & Conventions

- The main implementation is `CpalWavRecorder` in `src/lib.rs`.
- Capture state owns the CPAL stream, WAV writer, temp path, sample counter, and sample rate.
- ✅ DO: Normalize all input formats through `write_interleaved_mono()` before writing WAV samples.
- ✅ DO: Keep temp files under OS temp paths via `tempfile::Builder`, as in `start()`.
- ✅ DO: Remove temp files on setup failures; see cleanup in WAV creation, unsupported format, stream build, and stream play errors.
- ✅ DO: Return `RecordedAudio` with `content_type: "audio/wav"` and a file name derived from the temp path.
- ❌ DON'T: Add transcription, auth, or CLI behavior here; use `crates/codex-voice-codex` and `crates/codex-voice-app`.
- ❌ DON'T: Drop cleanup when adding a new CPAL sample format; every branch after temp creation must either store or remove the file.
- Keep the Linux-only `unsafe impl Send for CaptureState` narrow and documented.

## Touch Points / Key Files

- Recorder implementation: `src/lib.rs`
- Core audio trait: `crates/codex-voice-core/src/audio.rs`
- Audio doctor call site: `crates/codex-voice-app/src/main.rs`
- Root audio smoke command: `README.md`

## JIT Index Hints

```bash
rg -n "CpalWavRecorder|CaptureState|start\\(|stop\\(|cancel\\(" src/lib.rs
rg -n "SampleFormat|write_f32|write_i16|write_u16|write_interleaved_mono" src/lib.rs
rg -n "remove_file|tempfile|WavWriter|RecordedAudio" src/lib.rs
rg -n "doctor_audio|AudioDoctor" ../codex-voice-app/src/main.rs
```

## Common Gotchas

- CPAL streams are marked not sendable across every backend; do not broaden the current Linux-specific send assertion casually.
- `stop()` pauses briefly before finalizing the WAV; keep the audio smoke test when touching teardown.
- Short-recording discard lives in core, not this crate.

## Pre-PR Checks

```bash
cargo fmt --check && cargo check -p codex-voice-audio && cargo clippy -p codex-voice-audio --all-targets -- -D warnings && timeout 10s cargo run -p codex-voice-app --bin codex-voice -- doctor audio --seconds 1
```
