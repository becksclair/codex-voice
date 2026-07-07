# Plan 010: Remove the quadratic scan in split_tts_text

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-tts/src/client.rs`
> On drift, re-locate `split_tts_text` by grep; if its body no longer matches
> the excerpt, STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none (touches the same file as plans 009/011 — land in any order, rebase mechanically)
- **Category**: perf
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

`split_tts_text` gates every chunked TTS request. Its loop condition recomputes `remaining.chars().count()` — a full linear scan of the entire remaining string — on every iteration, while each iteration only consumes ~900 chars. Total work is O(n²/900): pure CPU burned on the synthesis hot path before any network work, growing quadratically with input length.

## Current state

- `crates/codex-voice-tts/src/client.rs:474-490`:

```rust
fn split_tts_text(input: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = input.trim();
    while remaining.chars().count() > max_chars {
        let split_at = split_index_at_or_before(remaining, max_chars);
        let (head, tail) = remaining.split_at(split_at);
        let head = head.trim();
        if !head.is_empty() {
            chunks.push(head.to_string());
        }
        remaining = tail.trim_start();
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}
```

- `split_index_at_or_before(input, max_chars) -> usize` (line ~492) — finds a byte index at or before the max_chars-th char (word/boundary-aware; read it, do not change it).
- Existing unit tests for splitting: `client.rs:594-609` (grep `split_tts_text` in the test module for the exact set). These pin the observable contract.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p codex-voice-tts split` | all splitting tests pass |
| Full crate tests | `cargo test -p codex-voice-tts` | all pass |
| Lint | `cargo clippy -p codex-voice-tts --all-targets -- -D warnings` | exit 0 |

## Scope

**In scope**:
- `fn split_tts_text` in `crates/codex-voice-tts/src/client.rs` (and its test module)

**Out of scope** (do NOT touch):
- `split_index_at_or_before` and every caller of `split_tts_text`.
- Chunk-boundary semantics: identical inputs must produce identical chunk vectors.

## Git workflow

- Branch: `advisor/010-fix-quadratic-split`
- One commit, e.g. `Make split_tts_text linear`.

## Steps

### Step 1: Replace the repeated count with a cheap loop condition

The key insight: the loop only needs to know whether MORE than `max_chars` chars remain — never the exact count. A `> max_chars` check needs at most `max_chars + 1` char steps, and it's cleaner still to avoid the recount entirely by checking whether a split actually consumed everything. Target shape:

```rust
fn split_tts_text(input: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = input.trim();
    // O(max_chars) check instead of O(len) count: is the (max_chars+1)-th char present?
    while remaining.chars().nth(max_chars).is_some() {
        let split_at = split_index_at_or_before(remaining, max_chars);
        let (head, tail) = remaining.split_at(split_at);
        let head = head.trim();
        if !head.is_empty() {
            chunks.push(head.to_string());
        }
        remaining = tail.trim_start();
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}
```

`chars().nth(max_chars).is_some()` is exactly equivalent to `chars().count() > max_chars` but bounded at `max_chars + 1` steps, making the whole function O(n). No other line changes.

**Verify**: `cargo test -p codex-voice-tts` → all pass, including every existing splitting test unchanged.

### Step 2: Add an equivalence + smoke test

In the same test module:

1. `split_matches_naive_count_semantics` — for a table of inputs (empty, exactly max, max+1, multibyte/emoji text, long whitespace runs), assert `split_tts_text(input, 10)` equals a locally-defined naive reference implementation using the old `chars().count() > max` condition. This proves behavioral equivalence.
2. `split_handles_large_input_quickly` — 500_000-char ASCII input with spaces, `max_chars = 900`; assert it completes and returns the expected chunk count. (No timing assertion — the test existing at all guards against reintroducing the quadratic pattern only weakly, but a timing assert would be flaky; the equivalence test is the real guard.)

**Verify**: `cargo test -p codex-voice-tts split` → all pass.

### Step 3: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

Step 2's equivalence-table test (multibyte chars are the named edge case — `chars()` vs bytes) and large-input smoke. Pattern: existing split tests at `client.rs:594-609`.

## Done criteria

- [ ] `grep -n "chars().count() > max_chars" crates/codex-voice-tts/src/client.rs` → no matches
- [ ] Equivalence test passes; all prior splitting tests unchanged and passing
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Any existing splitting test fails with the new condition (would mean the old condition wasn't equivalent for some input — report the counterexample).
- The function body at the location no longer matches the excerpt (plans 009/011 may have moved it — re-locate by name; if the logic itself changed, STOP).

## Maintenance notes

- `split_index_at_or_before` still walks up to `max_chars` chars per call — fine (bounded, linear overall). If anyone later changes chunk sizing to be dynamic, keep the loop condition bounded the same way.
- Reviewer should scrutinize: the multibyte rows of the equivalence table (that's where a byte/char confusion would hide).
