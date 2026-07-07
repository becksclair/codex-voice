# Plan 004: Pin dictation-engine error recovery and the Codex client's HTTP contract with tests

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-core/src/engine.rs crates/codex-voice-codex/src/client.rs`
> On drift, compare the "Current state" excerpts against the live code; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none (should land BEFORE plan 005, which refactors the engine's execution model)
- **Category**: tests
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

The dictation engine is the product's core state machine, and its error-recovery transitions — what happens when the mic fails to open, transcription errors, or paste fails — are untested. A change that leaves the engine stuck outside `Idle` after a failure would pass the suite today. Separately, the Codex transcription client's actual HTTP request assembly (bearer header, multipart body, non-2xx handling) has zero coverage — only the pure response parser is tested. Both gaps are cheap to close because the fakes and the mock-server pattern already exist in this repo. Plan 005 will refactor how the engine runs; these characterization tests must exist first.

## Current state

- `crates/codex-voice-core/src/engine.rs` (303 lines) — `DictationEngine` with `handle_hotkey` (line 85), `start` (93), `stop` (101), `process_recording` (121), `fail` (154: sends `AppEvent::Error` then `set_state(Idle)`).
  - Existing tests (line 168+): `discards_short_recordings`, `returns_to_idle_after_error` (covers ONE error path), `speak_selection_hotkey_does_not_change_dictation_state`.
  - Fakes already defined in the test module: `FakeAudio` (has a `start_error: bool` field — failure injection is already scaffolded), `FakeTranscription`, `FakeInjector`.
  - Untested arms: `start()`'s `Err` → `fail(ErrorStage::AudioStart, ..)` (line 97); `stop()`'s `Err` → `fail(ErrorStage::AudioStop, ..)` (line 118); `process_recording`'s empty-transcript → `Idle` (line 130), injector `Err` → `fail(ErrorStage::Insertion, ..)` (line 139), transcription `Err` → `fail(ErrorStage::Transcription, ..)` (line 144).
- `crates/codex-voice-codex/src/client.rs` — `CodexTranscriptionClient::transcribe` (line 36): `spawn_blocking` auth read, then a reqwest multipart POST to `TRANSCRIBE_URL` (const, line 10), timeout at line 23, response parsing via `parse_transcript` (line 111). Only `parse_transcript` has tests (lines 137, 145). **The URL is a hardcoded const** — testability requires making the base URL injectable (private, e.g. `with_base_url` used by tests, or storing the URL in the struct set from the const in `new`).
- Exemplar mock-server test to copy: `crates/codex-voice-transcriber/src/client.rs:225` (`discover_sends_bearer_token_to_health_probe`) — binds `tokio::net::TcpListener` on `127.0.0.1:0`, serves an axum `Router`, asserts the `Authorization` header.
- Conventions: `#[tokio::test]` for async tests; typed errors (`TranscriptionError`); no network beyond loopback; temp files via `tempfile`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Core tests | `cargo test -p codex-voice-core` | all pass (13 at planning + new) |
| Codex tests | `cargo test -p codex-voice-codex` | all pass (6 at planning + new) |
| Lint | `cargo clippy -p codex-voice-core -p codex-voice-codex --all-targets -- -D warnings` | exit 0 |
| Full gates | the four AGENTS.md commands | exit 0 |

## Scope

**In scope**:
- `crates/codex-voice-core/src/engine.rs` (test module only — production code unchanged)
- `crates/codex-voice-codex/src/client.rs` (test module + minimal URL-injection hook)
- `crates/codex-voice-codex/Cargo.toml` (dev-dependency on axum for the mock server, if not present)

**Out of scope** (do NOT touch):
- Engine production logic — if a test reveals a genuine bug (e.g. a path that does NOT return to `Idle`), STOP and report; do not fix it silently here.
- `crates/codex-voice-codex/src/auth.rs` (plan 002).
- `TRANSCRIBE_URL`'s value and the public constructor signatures (`new`, `with_timeout`) — the injection hook must be additive.

## Git workflow

- Branch: `advisor/004-engine-and-client-test-gaps`
- Commits: one for the engine tests, one for the client test + URL hook.

## Steps

### Step 1: Extend the engine fakes for full failure injection

In `engine.rs`'s `mod tests`: `FakeAudio` already has `start_error`. Add analogous switches where missing — `stop_error: bool` on `FakeAudio`, an error mode on `FakeTranscription` (e.g. `Option<String>` error), an `insert_error: bool` on `FakeInjector`, and an empty-transcript mode on `FakeTranscription`. Follow the existing fake style exactly (Mutex-wrapped state, `async_trait` impls).

**Verify**: `cargo test -p codex-voice-core` → existing 13 tests still pass.

### Step 2: Add the five error-path engine tests

Each test drives the engine with `handle_hotkey(Pressed)` / `handle_hotkey(Released)` and asserts on both the resulting state and the emitted `AppEvent`s (the tests receive events via the mpsc receiver the engine is constructed with — see `returns_to_idle_after_error` for the wiring):

1. `audio_start_failure_returns_to_idle_with_audio_start_stage` — `start_error: true`; expect `AppEvent::Error { stage: ErrorStage::AudioStart, .. }`, final state `Idle`, and a subsequent `Pressed` works (engine not wedged).
2. `audio_stop_failure_returns_to_idle_with_audio_stop_stage`.
3. `transcription_failure_deletes_recording_and_returns_to_idle` — also assert `AppEvent::RecordingDeleted` fires (the temp file cleanup at `process_recording` line 124 happens before the error branch).
4. `insertion_failure_returns_to_idle_with_insertion_stage`.
5. `empty_transcript_returns_to_idle_without_insertion` — whitespace-only transcript; assert the injector was never called (add a call-count to `FakeInjector`).

Use `tempfile::NamedTempFile` for recording paths as the existing tests do.

**Verify**: `cargo test -p codex-voice-core` → 18 tests pass (13 + 5).

### Step 3: Make the Codex client's endpoint injectable (test-only surface)

In `client.rs`: store the URL in the struct (`transcribe_url: String`, initialized to `TRANSCRIBE_URL` in both constructors). Add `#[cfg(test)] fn with_base_url_for_tests(auth: CodexAuthService, timeout: Duration, url: String) -> Self` (or make an existing constructor take it privately). Production behavior must be byte-identical: `grep TRANSCRIBE_URL` still resolves to the same const used by default.

**Verify**: `cargo check -p codex-voice-codex` → exit 0.

### Step 4: Add the HTTP contract test

Model on `crates/codex-voice-transcriber/src/client.rs:225`. Add dev-dependency `axum = { workspace = true }` to `crates/codex-voice-codex/Cargo.toml` under `[dev-dependencies]` if absent.

1. `transcribe_sends_bearer_and_multipart_and_parses_response`: loopback axum server whose handler asserts the `Authorization: Bearer <token>` header is present and the content type is multipart, then returns `{"text": "hello world"}` (check `parse_transcript` at line 111 for the exact expected response shape — read it first; the parser looks for the transcript field it validates in `parses_json_transcript_text`, line 137). Build a `CodexAuthService::with_auth_path` pointing at a temp auth.json fixture containing a fake token (see how existing auth tests construct fixtures, `auth.rs` test module). Assert the returned transcript equals the mock's text.
2. `transcribe_maps_non_2xx_to_error`: handler returns 500; assert `Err` of the expected `TranscriptionError` variant (read the error-mapping code in `transcribe` to name the exact variant before writing the assertion).

Use a real small WAV for the `RecordedAudio` fixture: write a minimal valid WAV via `hound` in the test (or copy the fixture approach used by `crates/codex-voice-audio/src/wav.rs` tests).

**Verify**: `cargo test -p codex-voice-codex` → all pass including 2 new tests.

### Step 5: Full workspace gates

**Verify**: all four AGENTS.md gates → exit 0.

## Test plan

This plan IS the test plan: 5 engine error-path tests + 2 client HTTP tests. Structural patterns: `engine.rs:168+` for engine tests; `transcriber/src/client.rs:225` for the mock server.

## Done criteria

- [ ] `cargo test -p codex-voice-core` → 18 passing tests
- [ ] `cargo test -p codex-voice-codex` → 8+ passing tests, incl. bearer/multipart contract and non-2xx mapping
- [ ] No production logic changes in `engine.rs`; only additive test hook in `client.rs` (`git diff` shows the URL field + cfg(test) constructor only)
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Any new test reveals the engine does NOT return to `Idle` on an error path — that is a production bug; report it with the failing test rather than changing engine code.
- `transcribe`'s request shape can't be exercised without modifying public constructor signatures.
- The fakes' trait signatures no longer match the current `AudioRecorder`/`TranscriptionClient`/`TextInjector` traits (drift).

## Maintenance notes

- Plan 005 (responsive event loop) will move engine execution onto its own task — these tests are its safety net and must keep passing unchanged there.
- The `#[cfg(test)]` URL hook is intentionally not a public API; if a config-driven endpoint is ever wanted (e.g. for a mock backend in doctor commands), promote it deliberately.
- Reviewer should scrutinize: event-assertion order in the engine tests (events are async mpsc — drain with timeouts rather than assuming ordering beyond what the engine guarantees).
