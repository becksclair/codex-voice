# Plan 009: Synthesize long-text TTS chunks with bounded concurrency

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-tts/src/client.rs`
> On drift, compare the "Current state" excerpt against the live code; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (provider rate limits; per-chunk fallback semantics must be preserved)
- **Depends on**: none (independent of plan 011; if 011 lands first, the loop lives in the same place — adapt)
- **Category**: perf
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

TTS inputs over 1,600 chars are split into ~900-char chunks and synthesized **serially** — each provider HTTP round-trip completes before the next starts, so latency scales linearly with text length. Long passages (the exact use case chunking exists for) wait N× a single round-trip when K-bounded concurrency would cut that to ~N/K. The same serial pattern exists in the embedded browser app's JS; that is explicitly out of scope here (it moves with the web-asset work).

## Current state

- `crates/codex-voice-tts/src/client.rs`:
  - Constants: `CHUNKED_TTS_MIN_CHARS = 1_600` (line 16), `CHUNKED_TTS_MAX_CHARS = 900` (line 17).
  - `synthesize_dispatch` — chunking branch at ~line 381: splits via `split_tts_text` (line 388), then the serial loop:

```rust
// client.rs:407-424 at planning
let mut synthesized_chunks = Vec::with_capacity(chunks.len());
for (index, chunk) in chunks.into_iter().enumerate() {
    tracing::debug!(provider = ?provider, chunk_index = index, ...);
    let chunk_request = SpeechRequest {
        input: chunk,
        format: chunk_format,
        ..request.clone()
    };
    synthesized_chunks.push(
        self.synthesize_single_with(provider, &chunk_request, persona, native_voice)
            .await?,
    );
}
```

- After the loop, chunks are concatenated (grep `concatenate_wav_chunks\|concatenate_pcm_chunks` in `convert.rs`) — concatenation requires **ordered** input.
- `synthesize_single_with(&self, provider, &SpeechRequest, persona, native_voice)` — takes `&self`; confirm `Self: Sync` holds (it holds reqwest clients — `reqwest::Client` is `Send + Sync`). Read its signature and the fallback behavior inside it before starting: per-chunk provider fallback must keep operating per chunk, unchanged.
- `futures-util` is a workspace dependency; add `futures-util.workspace = true` to `crates/codex-voice-tts/Cargo.toml` if absent.
- Test infra: this file has unit tests around splitting (`client.rs:594-609`); synthesis-path tests with fake providers may not exist — check `grep -n "mod tests" crates/codex-voice-tts/src/client.rs` and read what's there.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-tts` | exit 0 |
| Tests | `cargo test -p codex-voice-tts` | all pass (77 at planning) |
| Lint | `cargo clippy -p codex-voice-tts --all-targets -- -D warnings` | exit 0 |
| Live smoke (operator) | `cargo run -p codex-voice-app --bin codex-voice -- doctor tts --text "<~2000 chars>"` | audio plays, faster than before |

## Scope

**In scope**:
- `crates/codex-voice-tts/src/client.rs`
- `crates/codex-voice-tts/Cargo.toml` (futures-util dep only)

**Out of scope** (do NOT touch):
- The JS serial loops inside the embedded web app (`server.rs`/`assets/web/app.html`) — browser-direct synthesis is a separate surface.
- `split_tts_text` (plan 010 covers its complexity), `convert.rs` concatenators, provider clients (`google.rs`, `elevenlabs.rs`).
- Chunk-boundary silence/stitching behavior — whatever `synthesize_dispatch` does after collecting chunks stays byte-identical.

## Git workflow

- Branch: `advisor/009-concurrent-tts-synthesis`
- One commit, e.g. `Synthesize TTS chunks with bounded concurrency`.

## Steps

### Step 1: Bounded, ordered concurrent synthesis

Add a constant near the chunking consts:

```rust
/// Provider synthesis requests in flight per chunked TTS request.
const CHUNKED_TTS_CONCURRENCY: usize = 3;
```

Replace the serial loop:

```rust
use futures_util::stream::{self, StreamExt, TryStreamExt};

let synthesized_chunks: Vec<_> = stream::iter(chunks.into_iter().enumerate())
    .map(|(index, chunk)| {
        let chunk_request = SpeechRequest {
            input: chunk,
            format: chunk_format,
            ..request.clone()
        };
        async move {
            tracing::debug!(provider = ?provider, chunk_index = index, "synthesizing TTS chunk");
            self.synthesize_single_with(provider, &chunk_request, persona, native_voice)
                .await
        }
    })
    .buffered(CHUNKED_TTS_CONCURRENCY)
    .try_collect()
    .await?;
```

`buffered` (not `buffer_unordered`) preserves chunk order for concatenation. Borrowing `self`/`persona`/`native_voice` across the stream: the futures borrow `&self` — this compiles because `try_collect().await` completes within the enclosing scope. If lifetime errors arise from `chunk_request` being owned per-future while `persona`/`native_voice` are references, move the owned request in and keep the references (they outlive the await since they're parameters of the enclosing fn).

**Verify**: `cargo check -p codex-voice-tts` → exit 0; `cargo test -p codex-voice-tts` → all 77 pass.

### Step 2: Preserve first-error semantics knowingly

Today the serial loop aborts on the first failing chunk (after its own internal fallback attempts). `try_collect` on a `buffered` stream also fails on the first error (dropping in-flight siblings). Confirm `synthesize_single_with`'s internal fallback still runs per chunk by reading it — this plan must not change what happens *inside* a single chunk's synthesis.

**Verify**: code read confirms fallback is inside `synthesize_single_with`; no changes needed. Note the confirmation in your report.

### Step 3: Add ordering/concurrency tests

The provider clients are concrete structs (no trait seam at planning time — plan 011 adds one), so full fake-provider tests may not be feasible here. Do what IS feasible:

- If `mod tests` in `client.rs` already exercises `synthesize_dispatch` with any stub/mock: extend it with an ordered-output test (chunk N returns audio tagged N; assert concatenation order).
- If no such seam exists: add a unit test for the new stream logic in isolation — factor the ordered-buffered execution into a small generic helper `async fn synthesize_ordered<F, Fut, T, E>(items, concurrency, f) -> Result<Vec<T>, E>` in the same file and test THAT with closures (reversed latencies → output still ordered; active-counter high-water ≥2). The dispatch code then calls the helper.

Take the second path by default — it makes the ordering property testable without providers.

**Verify**: `cargo test -p codex-voice-tts` → all pass including new tests.

### Step 4: Full gates + operator smoke

**Verify**: the four AGENTS.md gates → exit 0. Note in your report that `doctor tts` with a >1,600-char text is the operator's live confirmation (requires real provider credentials — do not run it yourself unless the operator's `~/.codex/read-aloud-defaults.json` exists and the operator asked for a live check; per root AGENTS.md, TTS changes warrant a `doctor tts` smoke).

## Test plan

Step 3's helper tests (ordering under adversarial latency, concurrency high-water). All 77 existing tts tests unchanged.

## Done criteria

- [ ] No serial `for` loop awaiting `synthesize_single_with` per chunk remains
- [ ] Order-preservation test passes; concurrency test passes
- [ ] `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `synthesize_single_with` mutates shared state (e.g. interior-mutable failover bookkeeping that assumes serial execution) — read it first; if fallback state is shared across chunks, concurrency changes semantics and needs a design call.
- Lifetime/borrow errors require restructuring `synthesize_dispatch`'s signature.
- Any existing test's expected audio output changes (byte-level chunk concat differences would mean ordering broke).

## Maintenance notes

- Provider rate limits: ElevenLabs and Gemini both throttle; `CHUNKED_TTS_CONCURRENCY = 3` is conservative. If 429s appear, this constant is the knob.
- Plan 011 (TtsProvider trait) touches the same dispatch region — whichever lands second rebases carefully around the stream block.
- The browser-side serial loops (in the web app JS) remain; if the PWA becomes the primary surface, mirror this change there (deferred — see plans/README.md).
- Reviewer should scrutinize: `buffered` vs `buffer_unordered`, and that `request.clone()` per chunk doesn't clone large audio buffers (it's a text request — cheap).
