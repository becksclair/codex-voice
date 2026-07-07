# Plan 017: Minor robustness — recorder start-failure cleanup and poisoned-lock recovery

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-audio/src/recorder.rs crates/codex-voice-transcriber/src`
> On drift, re-locate by the greps given; on structural mismatch, STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none (server half composes with plan 006's file split — locate by name)
- **Category**: bug
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

Two small latent-failure cleanups. (a) When audio capture fails to start (stream build or `play()` error), the WAV writer thread has already been spawned and has already created the temp file; the early return drops the `TempWavGuard` (deleting the file) but the writer thread keeps running with an open handle, is never joined, its senders stay alive in dropped-closure limbo, and on some interleavings it recreates/finalizes the just-deleted path — leaking an orphan `codex-voice-*.wav` per failed start. (b) Server request handlers acquire shared locks with `.expect("...")`: one panic while holding a lock permanently poisons it, converting a single failure into a persistent panic loop on every subsequent request to that endpoint.

## Current state

- `crates/codex-voice-audio/src/recorder.rs`, `CpalWavRecorder::start` (~line 29):
  - Temp file + `TempWavGuard` created (~:44-52), then `writer_thread = std::thread::spawn(...)` which immediately does `WavWriter::create(&writer_path, spec)` (~:69-84), THEN the cpal stream is built (~:94-125) and `stream.play()` (~:126-128). Both stream steps early-return with `?`-style `map_err` on failure — after the writer thread is live. On success, `guard.keep()` and the state stores `writer_thread: Some(handle)` (~:130-135).
  - The writer loop exits when all `data_tx` senders drop (`while let Ok(chunk) = data_rx.recv()`), then calls `writer.finalize()` — i.e. it re-writes the file even after the guard deleted it.
- Server lock sites (`crates/codex-voice-transcriber/src/server.rs` at planning; post-006 in `src/server/*.rs`) — enumerate with:

```bash
grep -n '\.expect("TTS state lock")\|\.expect("web speech job store lock")' crates/codex-voice-transcriber/src/
# planning-time hits: 4456 (reload write), 4571/4638/5010/5075 (reads in health/web_config/web_speech_client/speech),
# 4937/4953/4975 (web speech job mutex in create/complete/status)
```

- Conventions: typed `AudioError` in the audio crate; `ApiError` in server handlers.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Audio tests | `cargo test -p codex-voice-audio` | all pass |
| Server tests | `cargo test -p codex-voice-transcriber` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |

## Scope

**In scope**:
- `crates/codex-voice-audio/src/recorder.rs`
- The server file(s) containing the grep hits above

**Out of scope** (do NOT touch):
- The recorder's success-path architecture (channel sizes, buffer pool, thread structure).
- Lock GRANULARITY or what the handlers do — only how lock acquisition failure is handled.

## Git workflow

- Branch: `advisor/017-minor-robustness`
- Commits: (1) recorder, (2) server locks.

## Steps

### Step 1: Reorder recorder startup so the writer thread spawns last

In `CpalWavRecorder::start`, move the `std::thread::spawn(writer_thread_closure)` to AFTER `stream.play()` succeeds. Concretely: create the channels where they are now; build the stream; `play()` it; only then spawn the writer thread (the closure already owns `writer_path`, `spec`, `data_rx`, `pool_tx` — move the spawn call, not the closure's contents). The cpal callbacks may push chunks into `data_tx` for the few microseconds before the writer starts consuming — the bounded(2048) channel absorbs that; no data loss, no behavior change.

On the early-return paths, nothing to clean up anymore: no thread was spawned and the guard's Drop deletes the temp file.

**Verify**: `cargo check -p codex-voice-audio && cargo test -p codex-voice-audio` → exit 0, 9 tests pass.

### Step 2: Regression test for the failure path

The stream-build failure is hardware-bound and can't be forced in a unit test portably. Test what IS testable: extract the "spawn writer thread" into a small named fn if useful, and add a test that exercises the writer thread lifecycle directly:

- `writer_thread_finalizes_only_after_senders_drop` — spawn the writer logic against a temp path, send a chunk, drop the sender, join, assert the file exists and is a valid WAV (parse with `hound::WavReader::open`), assert the sample count.
- `no_orphan_file_when_writer_never_spawned` — simulate the new failure ordering: create guard, do NOT spawn writer, drop guard, assert the temp path no longer exists.

Model on the existing tests in `crates/codex-voice-audio/src/wav.rs`'s test module (temp files, hound round-trips).

**Verify**: `cargo test -p codex-voice-audio` → all pass including 2 new.

### Step 3: Recover from poisoned locks in server handlers

Replace each `.expect("...")` grep hit with poison recovery — the guarded state is plain data (config snapshot / job map), valid even if a panicking thread held the lock:

```rust
// read/write RwLock:
let tts = state.tts.read().unwrap_or_else(std::sync::PoisonError::into_inner);
// Mutex:
let mut jobs = state.web_speech_jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
```

Apply uniformly at all grep hits (including the reload-path write at planning-line 4456 and `watch_tts_config`'s usages if they share the pattern — the grep is authoritative). Add one shared comment at the first site: poisoning is recovered because the state is a plain snapshot, not a partially-mutated invariant.

**Verify**: `cargo test -p codex-voice-transcriber` → all pass. The grep from Current state → zero remaining `.expect(` hits on those two lock messages.

### Step 4: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

Step 2's two recorder tests. The lock change is exercised by every existing server test (they all traverse these locks); no poisoning-specific test is practical without contriving a panic-under-lock — skip it and say so.

## Done criteria

- [ ] Writer thread spawns only after `stream.play()` succeeds
- [ ] 2 new audio tests pass; `cargo test --workspace` exits 0
- [ ] Zero `.expect("TTS state lock")` / `.expect("web speech job store lock")` remain
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- The writer-spawn reorder can't be done without restructuring the closure's captures (senders/pool wiring resists the move).
- Some lock site guards an actual multi-step invariant (not a snapshot) — `into_inner` there would be wrong; report the site.

## Maintenance notes

- If capture startup later becomes async or adds pre-roll buffering, keep the invariant: no writer thread before a confirmed-playing stream.
- The `into_inner` recovery pattern should be the crate default for these locks going forward; a future helper (`fn read_tts(state) -> Guard`) could centralize it if sites multiply.
- Reviewer should scrutinize: that the reorder didn't change the success path's ordering guarantees (state stored only after play, as before), and that every grep hit got the recovery treatment.
