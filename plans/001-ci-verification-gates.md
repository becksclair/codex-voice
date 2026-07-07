# Plan 001: Enforce the four verification gates with CI and mise tasks

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- mise.toml Cargo.toml .gitea .github`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

The repo defines four verification gates in `AGENTS.md` (fmt, check, test, clippy) and all four pass today, but nothing runs them automatically — no CI on either remote, no git hooks, no task runner entries. Any regression lands silently. Every other plan in `plans/` assumes these gates as its safety net, so enforcing them is the prerequisite for the rest of the backlog.

## Current state

- `mise.toml` — contains exactly one task, `[tasks.setup]` (builds the release binary and installs systemd user services). No fmt/check/test/lint tasks.
- No `.github/`, `.gitea/`, or `.forgejo/` directory exists.
- `git remote -v` shows two remotes: `origin git@git.heliasar.com:bex/codex-voice.git` (Gitea, primary) and `github git@github.com:becksclair/codex-voice.git` (mirror).
- The gates, per `AGENTS.md` "Root Setup Commands", all pass at the planned-at commit:

```bash
cargo fmt --check
cargo check --workspace
cargo test --workspace          # 177 passed, 0 failed, 1 ignored
cargo clippy --workspace --all-targets -- -D warnings
```

- Gitea Actions uses GitHub-Actions-compatible workflow YAML placed under `.gitea/workflows/`. GitHub reads `.github/workflows/`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Format check | `cargo fmt --check` | exit 0, no diff |
| Compile | `cargo check --workspace` | exit 0 |
| Tests | `cargo test --workspace` | exit 0, 177+ passed |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Task runner | `mise run verify` | (after Step 1) runs all four gates, exit 0 |

## Scope

**In scope** (the only files you should modify/create):
- `mise.toml`
- `.gitea/workflows/ci.yml` (create)
- `.github/workflows/ci.yml` (create)

**Out of scope** (do NOT touch):
- `Cargo.toml` — do not add `[workspace.lints]` in this plan; clippy is enforced via the CLI flag to keep this change config-only.
- `packaging/`, the `setup` task's body — installation flow is unrelated.
- Any Rust source file.

## Git workflow

- Branch: `advisor/001-ci-verification-gates`
- Commit style: imperative summary, matching repo history (e.g. `Add CI workflows enforcing verification gates`). Do NOT push unless the operator instructed it.

## Steps

### Step 1: Add gate tasks to mise.toml

Append to `mise.toml` (keep `[tasks.setup]` untouched):

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

**Verify**: `mise run verify` → all four gates run, exit 0.

### Step 2: Add the Gitea Actions workflow

Create `.gitea/workflows/ci.yml`:

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Install Linux build deps
        run: |
          sudo apt-get update
          sudo apt-get install -y libasound2-dev libgtk-3-dev libxdo-dev pkg-config
      - run: cargo fmt --check
      - run: cargo check --workspace
      - run: cargo test --workspace
      - run: cargo clippy --workspace --all-targets -- -D warnings
```

Note: the workspace links ALSA (`cpal`/`rodio`), GTK3 (`tray-icon`/`gtk`), and xdo (`arboard`/`tray-icon`) on Linux — the apt step is required or `cargo check` fails at the sys-crate build scripts. If the first CI run fails on a missing system library, add the corresponding `-dev` package to the apt list (that is an expected adjustment, not a STOP condition).

**Verify**: `python3 -c "import yaml,sys; yaml.safe_load(open('.gitea/workflows/ci.yml'))"` → exit 0 (well-formed YAML). If python3/yaml is unavailable, `mise x -- python3 ...` or visually confirm indentation.

### Step 3: Mirror the workflow for GitHub

Copy the identical file to `.github/workflows/ci.yml` (GitHub mirror gets the same gates).

**Verify**: `diff .gitea/workflows/ci.yml .github/workflows/ci.yml` → no output.

## Test plan

No new Rust tests — this plan adds enforcement, not behavior. The verification is that `mise run verify` passes locally and, after the operator pushes, the CI run is green on the Gitea remote.

## Done criteria

- [ ] `mise run verify` exits 0
- [ ] `.gitea/workflows/ci.yml` and `.github/workflows/ci.yml` exist and are identical
- [ ] `git status` shows no modified files outside the in-scope list
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `mise` is not installed or `mise run` errors for a reason unrelated to the new tasks.
- Any of the four gates FAILS at baseline (before your changes) — the tree has regressed since planning; report the failure output.
- The operator's Gitea instance turns out not to have Actions enabled (you cannot verify this locally — note it in your report so the operator checks).

## Maintenance notes

- When plans 002–017 land, this CI is what proves them. If a future plan adds a new gate (e.g. `cargo deny`), add it to both workflow files and `tasks.verify`.
- Deliberately deferred: `[workspace.lints]` in Cargo.toml (would change local build behavior), cargo-deny/cargo-audit advisories gating (operator preference needed on failure policy), and any release/packaging CI.
- Reviewer should scrutinize: the apt dependency list against what the runner actually needs (first green run is the proof).
