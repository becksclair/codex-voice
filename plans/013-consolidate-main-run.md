# Plan 013: Consolidate main.rs — one run loop, thin platform shims, testable orchestration

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-app/src`
> Plan 005 edits the same four run() bodies (engine → channel). This plan
> assumes 005 has LANDED; its excerpt references may differ accordingly —
> locate by name, and STOP only if the four-copy structure itself is gone.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED (per-platform bootstrap; only Linux is runtime-verifiable for the operator)
- **Depends on**: plans/005-responsive-event-loop.md
- **Category**: tech-debt
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

`main.rs` (794 lines) contains four `cfg`-gated near-copies of `async fn run()` (linux :110, windows :191, macos :274, fallback :355), plus three copies each of `open_logs()` (:678/:693/:752 + fallback :787) and `run_tray_diagnostics()` (:708/:731/:767 + fallback :792). A fix applied to the Linux bootstrap does not reach the other platforms unless someone remembers to copy it — and the speak/play orchestration (`run_speak_selection` :438, `run_speak_text` :473, `run_play_last_speech` :507, `synthesize_save_and_play` :552) lives in the binary where nothing can test it. The crate's own convention (`cli.rs` cleanly separated) shows the intended shape; this plan finishes the job.

## Current state

- `crates/codex-voice-app/src/main.rs` — the four `run()` variants differ in: which platform adapter types they construct (from `codex-voice-platform`), which `*UiConfig` they pass to `StatusTray::start`, and small platform quirks; the select-loop body is the near-identical core.
- Cross-platform seams that already exist and must be reused (not reinvented):
  - `codex-voice-core` traits: `HotkeyService`, `TextInjector`, `SelectedTextReader`, `PermissionService` (`crates/codex-voice-core/src/platform.rs:24-61`) — the platform crates implement these.
  - `codex_voice_ui::{StatusTray, UiCommand, UiStatus}` — identical API on all three platforms (see plan 012).
- Zero tests in `main.rs`, `tts.rs` (135 lines), `doctor.rs` (234 lines); the crate's 7 tests live in `cli.rs`/`logging.rs`.
- Before starting, read all four `run()` bodies in full and produce (in your working notes) a table of their actual differences — the consolidation is defined by that table, not by this plan's assumption. Expected differences: adapter construction, UiConfig type, log-path/diagnostics wiring.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-app` | exit 0 |
| Tests | `cargo test -p codex-voice-app` | all pass |
| Lint | `cargo clippy -p codex-voice-app --all-targets -- -D warnings` | exit 0 |
| Workspace | `cargo test --workspace` | all pass |
| Manual smoke (Linux) | `cargo run -p codex-voice-app --bin codex-voice -- run` | full dictation cycle works |

## Scope

**In scope**:
- `crates/codex-voice-app/src/main.rs`
- New modules in the app crate: `crates/codex-voice-app/src/app.rs` (shared run loop + orchestration), `crates/codex-voice-app/src/platform_shim.rs` (naming flexible; keep to two new files)

**Out of scope** (do NOT touch):
- `crates/codex-voice-core`, `codex-voice-ui`, `codex-voice-platform` — consolidation happens entirely in the app crate against existing seams.
- `cli.rs`, `doctor.rs`, `logging.rs`, `tts.rs` behavior.
- Any user-visible behavior: hotkey bindings, tray menu, log messages.

## Git workflow

- Branch: `advisor/013-consolidate-main-run`
- Commits: (1) extract shared loop, (2) platform shims + deletion of copies, (3) tests.

## Steps

### Step 1: Extract the shared run loop

Create `app.rs` with a platform-agnostic entry:

```rust
pub struct PlatformParts {
    // the trait objects / concrete generics each platform constructs:
    // hotkeys, injector, selected-text reader, audio recorder, transcription backend,
    // tray: Option<StatusTray>, log_path: PathBuf, ...
}

pub async fn run_app(parts: PlatformParts) -> anyhow::Result<()> {
    // the select-loop body common to all four run() variants, moved verbatim
}
```

Define `PlatformParts`'s exact fields from your difference table (Step 0 read). Where the four loops genuinely diverge (e.g. platform-specific diagnostics), inject behavior as a field (`diagnostics: fn(...)` or a small trait) rather than cfg-branching inside `run_app`.

Move `run_speak_selection`, `run_speak_text`, `run_play_last_speech`, `synthesize_save_and_play`, `play_audio_file`, `run_test_recording`, `run_tray_test_recording`, `spawn_status_task`, `status_sender_for_tray` into `app.rs` unchanged.

**Verify**: `cargo check -p codex-voice-app` → exit 0 (with the old `run()`s temporarily delegating or still present — keep the tree compiling).

### Step 2: Reduce each cfg run() to a shim

Each platform variant becomes ~15 lines: construct adapters + `StatusTray` + `PlatformParts`, then `run_app(parts).await`. Keep exactly one cfg-gated `open_logs`/`run_tray_diagnostics` per platform ONLY for the parts that truly differ (log-opening command, diagnostics content); if their bodies are near-identical, parameterize (e.g. the opener program name) and collapse.

Delete the four old loop bodies.

**Verify**: `cargo check -p codex-voice-app && cargo clippy -p codex-voice-app --all-targets -- -D warnings` → exit 0. `wc -l crates/codex-voice-app/src/main.rs` → expect well under 300.

### Step 3: Add orchestration tests

Now that `run_app`/orchestration fns live in a module, add `#[cfg(test)]` tests in `app.rs` using the core test-fake pattern (`crates/codex-voice-core/src/engine.rs` test module fakes; `crates/codex-voice-transcriber/src/test_support.rs`):

1. `run_app_quits_on_tray_quit_command` — fake tray channel delivering `UiCommand::Quit`; assert `run_app` returns.
2. `speak_text_reports_status_on_success_and_failure` — fake speech service; assert the `UiStatus` sequence sent to the status channel for both outcomes.
3. `test_recording_flow_produces_status_updates` — fake recorder.

These require `PlatformParts` to accept test doubles — which is exactly the design pressure that keeps the extraction honest. If a piece can't take a double without touching out-of-scope crates, leave it untested and note it.

**Verify**: `cargo test -p codex-voice-app` → all pass including new tests.

### Step 4: Full gates + smoke

**Verify**: the four AGENTS.md gates → exit 0. Manual Linux smoke (operator): full dictation cycle, tray menu items (test recording, speak text, play last, logs, diagnostics, quit) all function. Per root AGENTS.md, after runtime-service changes the operator must rebuild/restart user services — note this in your report.

## Test plan

Step 3's three orchestration tests; the existing 7 app tests unchanged; the engine/loop tests from plans 004/005 as regression net.

## Done criteria

- [ ] Exactly one select-loop body exists (`grep -c "tokio::select!" crates/codex-voice-app/src` → 1)
- [ ] `main.rs` under ~300 lines; per-platform `run()` shims construct-and-delegate only
- [ ] New orchestration tests pass; `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated (note manual-smoke status)

## STOP conditions

Stop and report back if:

- Plan 005 has not landed (this plan's loop excerpts assume the channel-driven engine).
- The difference table reveals the four loops diverge more than described (e.g. one platform handles extra event sources) — report the table before proceeding.
- `PlatformParts` needs more than ~10 fields or generics become unmanageable — the boxed-trait vs generic decision needs operator input.
- Windows/macOS cfg code cannot be compile-checked and the changes there are nontrivial — report which targets were verified.

## Maintenance notes

- New platform work now means writing one shim, not copying 80 lines of loop.
- The fallback (`not(any(linux, macos, windows))`) `run()` at :355 should become a shim with no tray — confirm what it does today before assuming.
- Reviewer should scrutinize: behavioral identity of the moved loop (diff should show motion, not edits) and that shims construct adapters in the same order/with the same config values as before.
