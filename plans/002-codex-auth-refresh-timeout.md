# Plan 002: Make the codex app-server auth refresh honor its timeout

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-codex/src/auth.rs`
> If the file changed since this plan was written, compare the "Current state"
> excerpts against the live code before proceeding; on a mismatch, treat it as
> a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

When the cached Codex auth is stale, the app spawns `codex app-server` and waits for an `account/read` JSON-RPC response on the child's stdout, with a 10-second deadline. The wait loop reads via a **blocking** `BufReader::read_line` on a standard pipe, so the deadline check only runs between complete lines. If the child writes a partial line, or stays alive producing nothing, `read_line` blocks forever, the deadline is never enforced, and the dictation flow hangs in "Transcribing" with a blocking-pool thread pinned until process exit. The code's own comment describes a non-blocking poll pattern that does not exist — `ErrorKind::WouldBlock` never occurs on std pipes, so that match arm is dead code.

## Current state

- `crates/codex-voice-codex/src/auth.rs` — Codex auth service. `refresh()` (line 68) spawns `codex app-server`, writes a request, then calls `wait_for_account_read(&mut child, stdout, Duration::from_secs(10))` (line 100).
- `wait_for_account_read` (lines 140–200) loops: `child.try_wait()` check, then deadline check, then the blocking read:

```rust
// auth.rs:164-196 (excerpt)
let remaining = deadline.saturating_duration_since(Instant::now());
if remaining.is_zero() {
    terminate_child(child);
    return Err(TranscriptionError::Auth(format!(
        "timed out after {}s waiting for codex account/read response",
        timeout.as_secs()
    )));
}

// Read one line with a timeout by setting a read timeout on the underlying handle.
// Since BufReader doesn't support timeouts directly, we use a short non-blocking poll
// pattern: try to read a line, and if we get WouldBlock, sleep briefly and retry.
match reader.read_line(&mut line) {
    Ok(0) => { std::thread::sleep(Duration::from_millis(50)); continue; }
    Ok(_) => {
        if is_account_read_response(&line) {
            terminate_child(child);
            return Ok(());
        }
        line.clear();
    }
    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(error) => { /* terminate_child + Auth error */ }
}
```

- Helper fns in the same file: `terminate_child(child: &mut Child)` (line 202), `is_account_read_response(line: &str)` (line 207).
- Callers: `read_or_refresh()` (line 103) calls `self.refresh()?`; the transcription client wraps `read_or_refresh` in `tokio::task::spawn_blocking` (`crates/codex-voice-codex/src/client.rs:37`), so `wait_for_account_read` runs on a blocking-pool thread — a hang there pins that thread permanently.
- Existing tests (`auth.rs:246-274`) cover `is_account_read_response` parsing only.
- Convention: this crate uses typed errors (`TranscriptionError`, thiserror) — keep that; do not introduce `anyhow`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-codex` | exit 0 |
| Tests | `cargo test -p codex-voice-codex` | all pass |
| Lint | `cargo clippy -p codex-voice-codex --all-targets -- -D warnings` | exit 0 |
| Full gates | `mise run verify` (if plan 001 landed) or the four AGENTS.md commands | exit 0 |

## Scope

**In scope**:
- `crates/codex-voice-codex/src/auth.rs`

**Out of scope** (do NOT touch):
- `crates/codex-voice-codex/src/client.rs` — the `spawn_blocking` wrapper is correct as-is.
- `crates/codex-voice-tts/src/codex_llm.rs` — its auth handling is covered by plan 003.
- The 10s timeout value and the request/response protocol — behavior-preserving fix only.

## Git workflow

- Branch: `advisor/002-codex-auth-refresh-timeout`
- One commit, e.g. `Enforce codex app-server refresh timeout with a reader thread`.

## Steps

### Step 1: Move the stdout read onto a dedicated reader thread

Rewrite `wait_for_account_read` so the deadline is authoritative:

1. Spawn a `std::thread` that owns `BufReader::new(stdout)` and loops `read_line`, sending each completed line over a `std::sync::mpsc::channel::<std::io::Result<String>>`. On `Ok(0)` (EOF) or send failure, the thread exits. The thread will also die naturally when the child is killed (read returns Err/EOF).
2. In the main loop, replace the direct `read_line` with `rx.recv_timeout(remaining.min(Duration::from_millis(100)))`:
   - `Ok(Ok(line))` → existing `is_account_read_response` handling.
   - `Ok(Err(io_error))` → existing read-error handling (`terminate_child` + `TranscriptionError::Auth`).
   - `Err(RecvTimeout::Timeout)` → loop back (re-checks `try_wait` and the deadline).
   - `Err(RecvTimeout::Disconnected)` → treat as EOF: sleep 50ms and continue (the existing `Ok(0)` behavior — the `try_wait`/deadline checks will exit the loop).
3. Keep the `child.try_wait()` and deadline checks exactly where they are at the top of the loop. On timeout, `terminate_child(child)` as today; the detached reader thread then unblocks on pipe EOF and exits — it must NOT be joined while the pipe is still open (that would reintroduce the hang). Detaching it (drop the JoinHandle) is acceptable and should be documented with a one-line comment stating why.
4. Delete the now-obsolete `WouldBlock` arm and the misleading comment block ("Read one line with a timeout ... poll pattern").

The function signature can stay `fn wait_for_account_read(child: &mut Child, stdout: impl std::io::Read, timeout: Duration)` — but note the reader thread needs `stdout` to be `Send + 'static`; change the bound to `impl std::io::Read + Send + 'static` and confirm the call site (line 100) still compiles (it passes an owned `ChildStdout`, which satisfies it).

**Verify**: `cargo check -p codex-voice-codex` → exit 0. `grep -n "WouldBlock" crates/codex-voice-codex/src/auth.rs` → no matches.

### Step 2: Add regression tests using in-process fake readers

Add to the existing `mod tests` in `auth.rs`. The tests exercise `wait_for_account_read` directly with a real spawned `Child` that behaves badly. Use a portable no-op child: `Command::new("sleep").arg("30")` with `Stdio::piped()` is NOT portable stdout — instead spawn `std::process::Command::new("cat")` (reads stdin forever, writes nothing to stdout) with piped stdio, pass its stdout, and assert the call returns an `Err` containing `"timed out"` within ~2× a short timeout (use `Duration::from_millis(300)`, not 10s). Measure with `Instant::now()` and assert `elapsed < Duration::from_secs(3)`.

Test cases:
1. `times_out_when_child_produces_no_output` — `cat` child as above; expect timeout error, and expect it promptly.
2. `times_out_on_partial_line_without_newline` — spawn `sh -c 'printf partial; sleep 30'`; the unterminated write must not defeat the deadline.
3. Keep all four existing parser tests passing unchanged.

These tests spawn `cat`/`sh`, which exist on any Linux/macOS CI runner. Guard both with `#[cfg(unix)]`.

**Verify**: `cargo test -p codex-voice-codex` → all pass, including the 2 new tests, in well under 30s total (if a test takes ≥30s, the hang is back — the fix is wrong).

### Step 3: Run the full workspace gates

**Verify**: `cargo fmt --check && cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → all exit 0.

## Test plan

Covered in Step 2: two hang-regression tests (silent child, partial-line child) plus the existing response-parsing tests. Pattern to follow for test structure: the existing `mod tests` at `auth.rs:240` (plain `#[test]`, no tokio needed — this is sync code).

## Done criteria

- [ ] `cargo test -p codex-voice-codex` exits 0 with 2 new timeout tests
- [ ] `grep -n "WouldBlock" crates/codex-voice-codex/src/auth.rs` → no matches
- [ ] New tests complete promptly (no test exceeds a few seconds)
- [ ] All four workspace gates pass
- [ ] No files outside `crates/codex-voice-codex/src/auth.rs` modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `wait_for_account_read` no longer matches the excerpt above (drift).
- The `Send + 'static` bound change breaks the call site in a way that requires modifying `refresh()`'s structure beyond passing the owned stdout.
- Tests hang or exceed 30s twice after a fix attempt.
- You find `wait_for_account_read` has other callers besides `refresh()` (there should be exactly one).

## Maintenance notes

- The reader thread is intentionally detached after timeout; if someone later adds a join, they must first ensure the child is dead (pipe closed), or the hang returns.
- If the codex CLI protocol changes (different response envelope), `is_account_read_response` is the only place to update; the threading here is protocol-agnostic.
- Reviewer should scrutinize: channel disconnect handling (EOF path) — it must still respect the deadline rather than spinning.
