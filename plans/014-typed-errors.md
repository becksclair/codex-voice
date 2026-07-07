# Plan 014: Restore the typed-error convention in the ui and transcriber crates

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-ui/src crates/codex-voice-transcriber/src/lib.rs crates/codex-voice-transcriber/src/discovery.rs`
> On drift, re-locate the signatures by grep; on structural mismatch, STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW
- **Depends on**: plans/012-dedupe-tray-implementations.md (soft — 012 hoists the `Result<_, String>` signatures this plan retypes; land 012 first to retype once)
- **Category**: tech-debt
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

Root `AGENTS.md` states the convention: "Prefer typed errors in library crates and `anyhow::Result` only at app/CLI boundaries." Two library crates violate it. The ui crate's public API is stringly-typed (`Result<Self, String>`), so callers can only string-match failures. The transcriber crate mixes typed `ApiError` in its HTTP layer with opaque `anyhow::Result` in public lib functions (`resolve_transcription_backend`, `probe_limits`, `write_discovery_file`), so the app boundary cannot distinguish recoverable from fatal transcriber failures. `codex-voice-core` (thiserror in 4 files) is the exemplar to match.

## Current state

- `crates/codex-voice-ui/` — no `thiserror` dependency. Stringly-typed sites (post-plan-012 some may live in `tray_common.rs`):
  - `linux_tray.rs:48` `pub fn start(...) -> Result<Self, String>`; `:87,:103-104` internal `Result<(), String>` ready-channels; `:395,:423` icon helpers → `Result<_, String>`; macos/windows equivalents (macos :48, :263, :291; windows :63, :406, :434).
  - Enumerate exhaustively with: `grep -rn "Result<[^,]*, String>" crates/codex-voice-ui/src/`
- `crates/codex-voice-transcriber/` — has typed `ApiError` (in server code) but:
  - `lib.rs:1` `use anyhow::{Context, Result};`, `lib.rs:85` `pub async fn resolve_transcription_backend() -> Result<ResolvedTranscriptionBackend>`, `lib.rs:114` `pub async fn probe_limits(...) -> Result<()>`.
  - `discovery.rs:131` `write_discovery_file(...) -> Result<()>` (anyhow).
  - `upload.rs:1` imports anyhow while its fns return `Result<_, ApiError>` — possibly an unused import; check.
  - Enumerate: `grep -rn "anyhow" crates/codex-voice-transcriber/src/ crates/codex-voice-transcriber/Cargo.toml`
- Exemplar for the target style: `crates/codex-voice-core/src/error.rs` if present, else the thiserror enums in core (grep `thiserror` in `crates/codex-voice-core/src/`) — match their naming (`AudioError`, `TranscriptionError` style) and `#[error(...)]` message conventions.
- Callers that must keep compiling: `crates/codex-voice-app/src/main.rs` consumes `StatusTray::start` (currently `.map_err`/string handling — grep `StatusTray::start` usage) and the transcriber lib fns (app is an anyhow boundary, so `?` with a typed error still works via `anyhow`'s `From<E: Error>` — provided the new error types implement `std::error::Error`, which thiserror gives you).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check --workspace` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |

## Scope

**In scope**:
- `crates/codex-voice-ui/src/*` + its `Cargo.toml` (add `thiserror.workspace = true`)
- `crates/codex-voice-transcriber/src/lib.rs`, `discovery.rs`, `upload.rs` (import cleanup) + its `Cargo.toml` (possibly remove anyhow)
- Minimal mechanical call-site adjustments in `crates/codex-voice-app` (e.g. removing a now-unneeded `.map_err(anyhow::Error::msg)`)

**Out of scope** (do NOT touch):
- `ApiError` and all HTTP-layer error mapping in the server (already typed and correct).
- `codex-voice-app`'s own use of `anyhow::Result` — that IS the sanctioned boundary.
- Error message TEXT — preserve existing messages verbatim inside the new variants where tests assert on them.

## Git workflow

- Branch: `advisor/014-typed-errors`
- Commits: (1) ui crate, (2) transcriber crate.

## Steps

### Step 1: Add `UiError` to the ui crate

In `lib.rs` (or `tray_common.rs` post-012):

```rust
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("tray initialization failed: {0}")]
    TrayInit(String),
    #[error("icon construction failed: {0}")]
    Icon(String),
    #[error("tray event loop failed: {0}")]
    EventLoop(String),
}
```

Choose variants from what the string errors actually describe (read each `Err(format!(...))`/`.map_err` site first; 3–5 variants, not one per message). Retype every `Result<_, String>` found by the scope grep to `Result<_, UiError>`, wrapping existing message strings in the appropriate variant. Update app call sites mechanically (anyhow absorbs `UiError` via `?` once it implements `Error`).

**Verify**: `cargo check --workspace` → exit 0. `grep -rn "Result<[^,]*, String>" crates/codex-voice-ui/src/` → no matches.

### Step 2: Type the transcriber's public lib surface

1. Define (in `lib.rs` or a new `error.rs`) a `TranscriberError` thiserror enum covering the failure classes of `resolve_transcription_backend`, `probe_limits`, and `write_discovery_file` (read each fn's `Context`/`bail` sites to pick variants: auth resolution, discovery I/O, backend probe, ffmpeg, etc.). Where these fns wrap errors from other crates, use `#[from]`/`#[source]` fields, preserving the context strings currently added via anyhow `Context`.
2. Retype the three public fns; remove `use anyhow` from files that no longer need it; if nothing in the crate uses anyhow afterwards, remove it from the crate's `Cargo.toml`.
3. `upload.rs:1`: delete the anyhow import if unused (clippy will confirm).

**Verify**: `cargo check --workspace && cargo test --workspace` → exit 0, all pass. `grep -rn "anyhow" crates/codex-voice-transcriber/src/` → no matches (or a documented residual with justification).

### Step 3: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

No new behavior. Existing tests must pass; where any test asserts on error message text, the preserved message strings keep them green. Add exactly two smoke assertions: one ui test constructing `UiError` variants and asserting `to_string()` output; one transcriber test asserting a representative `TranscriberError` display. (Pattern: core's error tests if present; else simple `#[test]` fns.)

## Done criteria

- [ ] No `Result<_, String>` in codex-voice-ui's source
- [ ] No `anyhow` in codex-voice-transcriber's source (or documented residual)
- [ ] `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Retyping forces a change to `ApiError` or any HTTP response shape.
- An app call site relies on matching specific error STRINGS (grep the app crate for `.contains(` on error values first) — that's a hidden contract; report it.
- The variant design balloons past ~6 variants per enum — the fns may be doing too much; report rather than inventing a taxonomy.

## Maintenance notes

- Future lib-crate fns should return the crate's typed error; `anyhow` stays quarantined in `codex-voice-app`. Consider a clippy lint or AGENTS.md note if violations recur.
- Plan 012 interaction: if 012 lands after this instead, it must hoist the *typed* signatures.
- Reviewer should scrutinize: that `#[from]` conversions don't silently swallow the context strings anyhow was adding — each `.context("...")` needs a home in a variant message.
