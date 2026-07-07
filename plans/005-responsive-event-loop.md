# Plan 005: Keep the tray and event loop responsive while dictation transcribes

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-app/src/main.rs crates/codex-voice-core/src/engine.rs`
> On drift, compare the "Current state" excerpts against the live code; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/004-engine-and-client-test-gaps.md (engine characterization tests must exist first)
- **Category**: bug
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

When the user releases the dictation hotkey, the engine transcribes the recording (a network round-trip with a 60s HTTP timeout) and pastes the result — and all of that is awaited **inline in the app's `tokio::select!` loop**. For the entire transcription window the loop cannot poll the tray (Quit is dead), cannot drain app events to update the tray icon, and cannot react to new hotkey presses. The app looks hung during its single most common operation. The fix: run the engine on its own task, fed by a channel, so the select loop only routes events.

## Current state

- `crates/codex-voice-app/src/main.rs` — four `cfg`-gated `async fn run()` variants: linux (line 110), windows (191), macos (274), fallback (355). Each builds the engine and runs a `tokio::select!` loop. The blocking call sites:

```rust
// main.rs:145 (linux; same shape at :228 windows, :309 macos, and in the fallback run())
other => engine.handle_hotkey(other).await,
```

- The loop also handles `SpeakSelectionPressed` by spawning a status task (non-blocking, line 141-144) — that pattern is the model: long work belongs on a spawned task.
- `crates/codex-voice-core/src/engine.rs` — `DictationEngine { audio, transcription, injector, events, state }`; `handle_hotkey(&mut self, event: HotkeyEvent)` (line 85) takes `&mut self`, so the engine cannot be shared across tasks as-is; it must be **moved onto** a dedicated task.
- `handle_hotkey` ignores events that don't match the current state (`_ => {}` at line 89), so single-flight semantics live inside the engine already — a queued `Pressed` during transcription is safely dropped by the same logic once the engine task processes it. Behavior nuance to preserve: today, a `Pressed` arriving *during* transcription is ignored (engine busy, state != Idle when handled inline). With a channel, that `Pressed` will be processed *after* transcription completes, when state is `Idle` again — meaning a hotkey press during transcription would now START a recording afterwards. To preserve current behavior, the engine task must drain/discard hotkey events that queued up while it was busy: after each `handle_hotkey` completes, loop `while let Ok(ev) = rx.try_recv() {}` to discard the backlog before awaiting the next event.
- Existing tests: engine tests in `engine.rs` (18 after plan 004); `main.rs` has none (no test harness needed for this change beyond the engine suite + manual smoke).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check --workspace` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Manual smoke (Linux) | `cargo run -p codex-voice-app --bin codex-voice -- run` | tray stays interactive during a dictation |

## Scope

**In scope**:
- `crates/codex-voice-app/src/main.rs`
- `crates/codex-voice-core/src/engine.rs` (only if a small helper — e.g. a `run_engine_loop` consuming a receiver — fits better in core; keep changes additive)

**Out of scope** (do NOT touch):
- Consolidating the four `run()` copies into one — that is plan 013. Apply the same mechanical change to each copy here.
- Engine state-machine semantics (which transitions exist) — behavior must be preserved, verified by the plan-004 tests.
- Tray implementations in `crates/codex-voice-ui/`.

## Git workflow

- Branch: `advisor/005-responsive-event-loop`
- One commit, e.g. `Run dictation engine on its own task to keep the event loop responsive`.

## Steps

### Step 1: Add an engine task helper

Create (in `engine.rs`, next to the engine) a free async fn:

```rust
/// Drives the engine from a hotkey-event channel. Events that arrive while a
/// transition is in flight are discarded to preserve inline-handling semantics.
pub async fn run_engine_loop(
    mut engine: DictationEngine</* same generics/bounds as construction sites */>,
    mut hotkeys: tokio::sync::mpsc::Receiver<HotkeyEvent>,
) {
    while let Some(event) = hotkeys.recv().await {
        engine.handle_hotkey(event).await;
        while hotkeys.try_recv().is_ok() {}
    }
}
```

Match the engine's actual generic signature (read the construction site in `main.rs` around each `run()` to get the concrete types; the fn can be generic exactly as `DictationEngine` is).

**Verify**: `cargo check -p codex-voice-core` → exit 0.

### Step 2: Rewire each run() variant

In all four `run()` copies in `main.rs` (lines 110, 191, 274, 355):

1. Create `let (engine_tx, engine_rx) = tokio::sync::mpsc::channel::<HotkeyEvent>(16);`
2. `tokio::spawn(run_engine_loop(engine, engine_rx));` instead of keeping `engine` in the loop.
3. Replace `other => engine.handle_hotkey(other).await,` with `other => { let _ = engine_tx.try_send(other); }` — `try_send` (not `send().await`) so a full channel never blocks the select loop; a dropped event under pathological backlog matches the drain-discard semantics.
4. `SpeakSelectionPressed` handling and everything else in the select loop stays unchanged.

Apply the identical edit to all four variants; they differ only in surrounding platform setup.

**Verify**: `cargo check --workspace` → exit 0. `grep -n "handle_hotkey" crates/codex-voice-app/src/main.rs` → no matches (all engine driving now goes through the channel).

### Step 3: Test the drain-discard semantics

In `engine.rs`'s test module, add:

1. `engine_loop_discards_hotkeys_queued_during_transition` — build an engine whose `FakeTranscription` delays (e.g. `tokio::time::sleep(50ms)` inside the fake, or a Notify-gated fake). Send `Pressed`, `Released`, then immediately `Pressed` again while the transcription sleep holds. After the loop drains, assert final state is `Idle` (the trailing `Pressed` was discarded, no new recording started — inspect via the events channel: exactly one `StateChanged(Recording)` occurred).
2. `engine_loop_processes_sequential_dictations` — two full Press/Release cycles with settled awaits between them; assert two complete transcription flows.

Run the loop under `tokio::time::pause()` or real short sleeps — follow whichever style existing async tests in the repo use (they use real short operations; keep sleeps ≤50ms).

**Verify**: `cargo test -p codex-voice-core` → all pass including the 2 new tests.

### Step 4: Full gates + manual smoke

**Verify**: all four AGENTS.md gates → exit 0. Then (operator machine, Linux): `cargo run -p codex-voice-app --bin codex-voice -- run`, hold Ctrl-M, speak a sentence, release, and while "Transcribing" is shown open the tray menu — it must respond, and Quit must work mid-transcription. Report this as "manual smoke: pending operator" if no display/portal session is available to you.

## Test plan

Step 3's two loop tests plus the 18 existing engine tests (plan 004) which must pass unchanged — they are the proof that engine semantics survived the execution-model change.

## Done criteria

- [ ] `grep -n "handle_hotkey" crates/codex-voice-app/src/main.rs` → no matches
- [ ] `run_engine_loop` exists and is driven via `tokio::spawn` in all four `run()` variants
- [ ] `cargo test --workspace` exits 0 (incl. 2 new loop tests, 18 prior engine tests untouched)
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated (note "manual smoke pending" if applicable)

## STOP conditions

Stop and report back if:

- The engine's generics make `run_engine_loop`'s signature explode (more than ~5 type params to thread through) — report; a boxed-trait constructor may be the better shape and needs a design call.
- Plan-004 tests fail after the rewire — semantics changed; do not adjust the tests to pass.
- The four `run()` variants have diverged from each other in how they drive the engine (drift from the excerpt).

## Maintenance notes

- Plan 013 (main.rs consolidation) will collapse the four edited sites into one — land this first so consolidation moves already-correct code.
- The drain-discard loop means a user who presses the hotkey during transcription gets nothing (matching old behavior). If the product decision ever changes to "queue the next dictation", delete the `try_recv` drain — that is the single point controlling it.
- Reviewer should scrutinize: channel capacity (16 is arbitrary but ample), and that `engine_tx` is not dropped early (it must live as long as the select loop or the engine task ends).
