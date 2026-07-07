# Plan 018: Make the mise verify tasks trustworthy inside nested git worktrees

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat d33b069..HEAD -- mise.toml AGENTS.md`
> If `mise.toml` changed since this plan was written, compare the "Current
> state" excerpt against the live code before proceeding; on a mismatch, treat
> it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/001-ci-verification-gates.md (DONE — added the gate tasks this plan fixes)
- **Category**: dx
- **Planned at**: commit `d33b069`, 2026-07-07

## Why this matters

`mise.toml` defines the verification gate tasks (`fmt`, `check`, `test`, `lint`, `verify`). By default, mise runs a task with its working directory set to the directory of the `mise.toml` that defines it — the **repository root**, not the directory mise was invoked from. This is invisible in normal use (you invoke from the repo root anyway), but it silently breaks verification inside a **git worktree nested under the repo** (this project's tooling creates worktrees under `.claude/worktrees/`). From such a worktree, `mise run verify` walks up, finds the root `mise.toml`, and runs `cargo …` rooted at the **primary checkout** — so it verifies the wrong tree and reports false-green.

This was observed directly while executing plans 002–004 in worktrees: `mise run verify` reported `cargo fmt --check` passing while the worktree's code was actually unformatted, and dependency resolution in the mise-run gate differed from the worktree's own lockfile (crate paths pointed at the primary checkout). The consequence is that any worktree-based verification (the plan-execution workflow, or manual worktree development) cannot trust the mise gate — every executor had to fall back to running `cargo` directly. Fixing this makes the one-command gate reliable everywhere.

## Current state

- `mise.toml` — after plan 001, contains `[tasks.setup]` plus the five gate tasks. The gate tasks have **no `dir` field**, so they inherit mise's default (config-root) working directory:

```toml
[tasks.fmt]
description = "Check formatting"
run = "cargo fmt --check"

[tasks.check]
description = "Type-check the workspace"
run = "cargo check --workspace"

[tasks.test]
description = "Run the workspace test suite"
run = "cargo test --workspace"

[tasks.lint]
description = "Clippy with warnings denied"
run = "cargo clippy --workspace --all-targets -- -D warnings"

[tasks.verify]
description = "Run all verification gates"
depends = ["fmt", "check", "test", "lint"]
```

- `[tasks.setup]` (the first task in the file) installs systemd services and is **out of scope** — it intentionally operates on the repo root; do not add a `dir` to it.
- mise version on this host: 2026.5.x. mise's task templating (tera) exposes a `{{cwd}}` variable that resolves to the directory mise was invoked from. Setting `dir = "{{cwd}}"` on a task makes it run in the invocation directory instead of the config root.
- CI is unaffected: the GitHub/Gitea Actions runners (`.gitea/workflows/ci.yml`, `.github/workflows/ci.yml`) check out a clean, non-nested tree and invoke `cargo` directly (not via mise), so the trap never occurs there. Do not modify the CI workflows.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Show a task's config | `mise tasks info fmt` | prints the task; after the fix, shows the `dir` |
| Create a nested test worktree | `git worktree add .claude/worktrees/mise-trap-test HEAD` | creates it |
| Remove the test worktree | `git worktree remove .claude/worktrees/mise-trap-test` | removes it |
| Run a gate from a worktree | `mise run fmt` (with cwd inside the worktree) | see Step 2 |

## Scope

**In scope**:
- `mise.toml` (add `dir` to the four gate tasks)
- `AGENTS.md` (a one-line note under "Definition of Done" — only in the fallback of Step 3)

**Out of scope** (do NOT touch):
- `[tasks.setup]` in `mise.toml`.
- `.gitea/workflows/ci.yml`, `.github/workflows/ci.yml` — CI is unaffected.
- Any Rust source, any other config.
- The harness's worktree location (`.claude/worktrees/`) — you cannot change where worktrees are created; the fix must be project-side.

## Git workflow

- Branch: `advisor/018-mise-worktree-verify`
- One commit, e.g. `Run mise gate tasks in the invocation dir so worktree verification is correct`.

## Steps

### Step 1: Reproduce the trap (baseline — confirm it exists before fixing)

1. From the repo root, create a nested worktree: `git worktree add .claude/worktrees/mise-trap-test HEAD`.
2. Introduce a deliberately mis-formatted line in a Rust file **inside that worktree only**, e.g. append a badly-indented throwaway function to some source file in the worktree (any change `cargo fmt --check` will flag). Do NOT commit it.
3. With your shell's working directory inside the worktree (`cd .claude/worktrees/mise-trap-test`), run `mise run fmt`.
4. **Observe the bug**: it reports success (exit 0) even though the worktree's code is mis-formatted — because it checked the parent tree. Record this output as the baseline proof.

If instead `mise run fmt` already FAILS here (detects the worktree's bad formatting), the trap does not reproduce in this environment/mise version — **STOP and report** (the premise may no longer hold; do not apply a fix for a bug that isn't there). Clean up the test worktree first (`git worktree remove --force .claude/worktrees/mise-trap-test`).

### Step 2: Apply the `dir` fix and verify it closes the trap

1. In `mise.toml`, add `dir = "{{cwd}}"` to each of `[tasks.fmt]`, `[tasks.check]`, `[tasks.test]`, `[tasks.lint]`. Leave `[tasks.verify]` (the aggregate) and `[tasks.setup]` unchanged — `verify` only declares `depends`, and each dependency now carries its own `dir`.

Example for one task:

```toml
[tasks.fmt]
description = "Check formatting"
dir = "{{cwd}}"
run = "cargo fmt --check"
```

2. Re-run the Step 1 reproduction: with cwd inside `.claude/worktrees/mise-trap-test` (which still has the mis-formatted line), run `mise run fmt`. It must now **FAIL** (exit non-zero, reporting the worktree's bad formatting) — proving the gate now tests the worktree.
3. Confirm the normal case still works: from the **repo root**, run `mise run fmt` → exit 0 (root tree is clean).
4. Remove the throwaway change and the test worktree: `git worktree remove --force .claude/worktrees/mise-trap-test`, then `git worktree prune`.

**Verify**: Step 2.2 fails (worktree change detected) AND Step 2.3 passes (root clean). Both must hold. If Step 2.2 still passes (trap not closed by `{{cwd}}`), go to Step 3.

### Step 3 (FALLBACK — only if Step 2 did not close the trap): document the limitation instead

If `dir = "{{cwd}}"` does not make the worktree gate detect worktree changes (mise resolves the template differently than expected in this version):

1. Revert the `mise.toml` changes (`git checkout mise.toml`).
2. Add a single note under the "Definition of Done" section of `AGENTS.md`: that verification inside a nested git worktree must use `cargo` directly (`cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`), NOT `mise run verify`, because mise resolves gate-task working directories to the primary checkout.
3. Report that the config fix did not work and the documentation fallback was applied instead.

Do not invent an alternative mise mechanism beyond `dir = "{{cwd}}"` without evidence it works — the empirical worktree check in Step 2 is the arbiter.

### Step 4: Final gates (root)

From the repo root (normal invocation), confirm nothing regressed:

**Verify**: `mise run verify` → exit 0 (all four gates pass on the clean root tree). If Step 3's fallback was taken, run the four `cargo` commands directly instead and confirm exit 0.

## Test plan

No Rust tests. The verification is behavioral and lives in Step 2: the same worktree gate that reported false-green before the fix must report the real (failing) state after it, while the root invocation stays green. That before/after asymmetry is the proof.

## Done criteria

- [ ] The four gate tasks (`fmt`, `check`, `test`, `lint`) carry `dir = "{{cwd}}"` in `mise.toml` (OR, if the fallback was taken, `AGENTS.md` documents the direct-cargo requirement and `mise.toml` is unchanged)
- [ ] From a nested worktree with a local mis-format, `mise run fmt` FAILS (detects it) — recorded in the report
- [ ] From the repo root, `mise run verify` exits 0
- [ ] No test worktree or throwaway change left behind (`git worktree list` shows only the primary; `git status` clean of the throwaway)
- [ ] Only `mise.toml` (and `AGENTS.md` in the fallback) modified
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- The trap does not reproduce in Step 1 (the gate already tests the worktree correctly) — report; there is nothing to fix.
- `mise.toml` no longer matches the "Current state" excerpt (drift since planning).
- `git worktree add` under `.claude/worktrees/` fails or that path is not writable in your environment — report; the reproduction requires a nested worktree.
- Applying `dir` breaks the root-invocation case (Step 2.3 fails) — do not ship a fix that regresses the normal path.

## Maintenance notes

- If future gate tasks are added to `mise.toml` (e.g. a `cargo deny` advisories task), give them the same `dir = "{{cwd}}"` so they inherit the correct behavior in worktrees.
- The `[tasks.setup]` task deliberately does NOT get `dir = "{{cwd}}"` — it installs system services and is meant to run against the repo root regardless of cwd.
- If the fallback (documentation only) was taken, revisit whenever mise is upgraded — a newer version may resolve `{{cwd}}` as expected, at which point the config fix is preferable to the convention.
- Reviewer should scrutinize: that the before/after worktree reproduction was actually run (not just the config edited), since the whole value of this plan is the empirical proof that the gate now tests the right tree.
