# Plan 008: Transcribe oversized-upload chunks with bounded concurrency

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-transcriber/src`
> If plan 006 landed, `transcribe_chunked` lives in `src/server/transcribe.rs`
> — expected, locate by name. On a mismatch with the excerpt, STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (upstream rate limits — concurrency must be bounded and ordering preserved)
- **Depends on**: none (coexists with 006; grep-locate the function)
- **Category**: perf
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

Uploads over the per-request Codex limit are split by ffmpeg into up to `MAX_GENERATED_CHUNKS = 512` WAV chunks, then transcribed **one at a time** — each `.await` completes a full network round-trip before the next starts. Wall-clock for a long recording is the sum of all chunk latencies; with bounded concurrency it approaches `ceil(N/K) × latency`. For the operator's real use (feeding `summarize` with multi-hundred-MB audio), this is minutes versus tens of minutes.

## Current state

- `crates/codex-voice-transcriber/src/server.rs` (post-006: `src/server/transcribe.rs`), `transcribe_chunked`:

```rust
// server.rs:4848-4856 at planning
let mut transcripts = Vec::with_capacity(chunks.paths.len());
for path in &chunks.paths {
    let filename = upload::filename_for_path(path);
    transcripts.push(
        client::transcribe_path(state.backend.as_ref(), path, &filename, "audio/wav")
            .await
            .map_err(|error| ApiError::backend(error.to_string()))?,
    );
}
Ok(upload::join_transcripts(&transcripts))
```

- `upload::join_transcripts(&[String]) -> String` requires transcripts **in chunk order** (`crates/codex-voice-transcriber/src/upload.rs` — read it before starting).
- `chunks.paths: Vec<PathBuf>` comes from `chunking.rs` (`MAX_GENERATED_CHUNKS = 512` at `chunking.rs:30`), ordered.
- `state.backend` is behind `Arc` (`ServiceState` — grep `struct ServiceState`); confirm `backend`'s type is `Arc<dyn ...>`/cloneable before fanning out. `client::transcribe_path` signature: grep `fn transcribe_path` in `crates/codex-voice-transcriber/src/client.rs`.
- `futures-util` is a workspace dependency (root `Cargo.toml`), providing `stream::iter` + `buffered`.
- Test infra: `test_support.rs` provides fake backends; the test module has chunked-path tests — grep `chunk` in the test module to find them.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-transcriber` | exit 0 |
| Tests | `cargo test -p codex-voice-transcriber` | all pass |
| Lint | `cargo clippy -p codex-voice-transcriber --all-targets -- -D warnings` | exit 0 |

## Scope

**In scope**:
- The file containing `transcribe_chunked`
- `crates/codex-voice-transcriber/Cargo.toml` (add `futures-util.workspace = true` if not already a dependency)
- The transcriber test module

**Out of scope** (do NOT touch):
- `chunking.rs` (chunk generation), `upload.rs` (`join_transcripts` and multipart parsing).
- The TTS synthesis path — that is plan 009.
- Retry/backoff logic — do not add any; error semantics stay fail-fast (first chunk error aborts the request, as today).

## Git workflow

- Branch: `advisor/008-concurrent-chunk-transcription`
- One commit, e.g. `Transcribe upload chunks with bounded concurrency`.

## Steps

### Step 1: Add a concurrency constant

Next to the function (or with the other consts in the module):

```rust
/// Upstream transcription requests in flight per chunked upload.
const CHUNK_TRANSCRIBE_CONCURRENCY: usize = 4;
```

4 is deliberate: enough to hide latency, small enough not to trip upstream rate limits. Do not exceed 8 without operator sign-off.

### Step 2: Replace the serial loop with an ordered buffered stream

```rust
use futures_util::stream::{self, StreamExt, TryStreamExt};

let transcripts: Vec<String> = stream::iter(chunks.paths.iter())
    .map(|path| {
        let filename = upload::filename_for_path(path);
        let backend = state.backend.clone(); // adjust to the real sharing shape
        async move {
            client::transcribe_path(backend.as_ref(), path, &filename, "audio/wav")
                .await
                .map_err(|error| ApiError::backend(error.to_string()))
        }
    })
    .buffered(CHUNK_TRANSCRIBE_CONCURRENCY)
    .try_collect()
    .await?;
Ok(upload::join_transcripts(&transcripts))
```

Key property: `buffered` (NOT `buffer_unordered`) preserves input order, so `join_transcripts` receives chunks in sequence. Adjust the `backend` capture to the actual field type: if `state.backend` is `Arc<dyn TranscriptionBackend>` clone the Arc; if the existing code borrows it fine across awaits, borrowing in the closure may work as-is since `chunks.paths` outlives the stream. Fail-fast on first error via `try_collect` matches today's `?` semantics (in-flight siblings get dropped/cancelled — acceptable).

**Verify**: `cargo check -p codex-voice-transcriber` → exit 0. Existing chunked-path tests pass: `cargo test -p codex-voice-transcriber chunk` → pass.

### Step 3: Add an ordering + concurrency regression test

Using the fake backend in `test_support.rs` (extend it if needed):

1. `chunked_transcripts_join_in_order_under_concurrency` — fake backend that returns transcript `"part-N"` for chunk N but with **reversed latency** (first chunk slowest: e.g. sleep `(count - index) * 10ms` inside the fake). Feed ≥4 chunks; assert the joined output is `part-0 part-1 part-2 ...` (exact join format per `join_transcripts` — read it). This fails under `buffer_unordered` and under any ordering bug.
2. `chunked_transcription_runs_concurrently` — fake backend records timestamps (or an active-counter high-water mark via `AtomicUsize`); with 4 chunks × 50ms sleeps, assert either max-active ≥2 or total elapsed < 150ms (serial would be ≥200ms).

Wire the test through whatever entry the existing chunked tests use (HTTP-level `oneshot` with an oversized synthetic upload, or direct `transcribe_chunked` call if visible to tests).

**Verify**: `cargo test -p codex-voice-transcriber` → all pass including 2 new.

### Step 4: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

Step 3's two tests, plus all existing chunked/oversize tests (413 behavior, ffmpeg-missing error) unchanged. Structural pattern: existing fakes in `test_support.rs`.

## Done criteria

- [ ] No serial `for ... { transcribe_path(...).await }` loop remains in `transcribe_chunked`
- [ ] Ordering test and concurrency test pass
- [ ] `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `state.backend` cannot be shared across concurrent futures without a locking change (e.g. it requires `&mut`) — that needs a design decision, not improvisation.
- `join_transcripts` turns out to be order-insensitive in a way that contradicts this plan (unlikely — verify by reading it; if so, note it and proceed, the ordered stream is still correct).
- The concurrency test is flaky twice in a row — report timings rather than loosening the assertion arbitrarily.

## Maintenance notes

- If the upstream Codex endpoint starts rate-limiting (429s appearing in logs), `CHUNK_TRANSCRIBE_CONCURRENCY` is the knob; consider making it config/env-driven then, not now.
- Fail-fast means one bad chunk aborts a long job late; a retry-per-chunk policy is a possible follow-up, deliberately out of scope.
- Reviewer should scrutinize: `buffered` vs `buffer_unordered` (ordering), and that error cancellation doesn't leak temp chunk files (chunk cleanup lives with the chunking guard — confirm the existing Drop/cleanup path in `chunking.rs` still runs; it is owned outside this loop).
