# Plan 003: Harden auth.json rewrites in the Codex LLM client (atomic private write, unique tmp, off-runtime I/O)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-tts/src/codex_llm.rs crates/codex-voice-core/src/fs.rs`
> On any drift, compare the "Current state" excerpts against the live code; on
> a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

The Codex LLM client (used for TTS speech-prep) refreshes OAuth tokens and rewrites `~/.codex/auth.json`. Three defects share one code region:

1. The temp file is created with `std::fs::write` (default umask, typically 0644) and only chmod'd to 0600 afterwards — with the error discarded (`let _ =`). The refresh-token file can be left world-readable.
2. The temp filename is keyed only by PID (`.auth.json.<pid>.tmp`). Two concurrent speech-prep requests in the same server process that both detect an expired token write the **same** temp path concurrently, then both rename it over `auth.json` — interleaved writes can corrupt the auth file and break all Codex auth until re-login. This is a real single-user bug (the server handles requests in parallel), independent of any security posture.
3. All this file I/O (`read_to_string`, `create_dir_all`, `write`, `rename`) runs synchronously inside async fns on the tokio runtime, stalling worker threads. The transcription path already does this correctly via `spawn_blocking` (`crates/codex-voice-codex/src/client.rs:37`).

A correct helper already exists in core: `codex_voice_core::fs::write_private_file_atomic` creates the temp file with mode 0600 via `O_CREAT|O_EXCL`, then renames.

## Current state

- `crates/codex-voice-tts/src/codex_llm.rs` — Codex Responses-endpoint client for speech prep.
  - `tokens(&self, force_refresh: bool)` (line 133): `read_auth_file(...)` → possibly `refresh_tokens(...).await` → `write_auth_file(...)`. Called from `generate_text` (line 47).
  - `read_auth_file` (line ~164): `std::fs::read_to_string` + serde parse.
  - `write_auth_file` (line ~271): `create_dir_all`, then:

```rust
// codex_llm.rs:275-296 (excerpt)
let tmp_path = path.with_file_name(format!(
    ".{}.{}.tmp",
    path.file_name().and_then(|n| n.to_str()).unwrap_or("auth.json"),
    std::process::id()
));
let text = serde_json::to_string(payload)...;
std::fs::write(&tmp_path, format!("{text}\n")).map_err(...)?;
#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
}
std::fs::rename(&tmp_path, path).map_err(...)?;
```

- `crates/codex-voice-core/src/fs.rs` — `write_private_file(path, bytes)` (0600 via `create_new` + `mode(0o600)`) and `write_private_file_atomic(path, tmp_path, bytes)` ("The caller must choose a unique `tmp_path` (e.g. by including a random suffix)"). Exemplar caller: `crates/codex-voice-transcriber/src/discovery.rs:131` (`write_discovery_file`).
- The tts crate already depends on `codex-voice-core` (check `crates/codex-voice-tts/Cargo.toml`; if the dependency is missing, add it via `codex-voice-core = { path = "../codex-voice-core" }` matching how other crates declare it).
- Error convention: this crate uses typed `SpeechError` — map I/O failures to `SpeechError::Auth(...)` as the current code does.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-tts` | exit 0 |
| Tests | `cargo test -p codex-voice-tts` | all pass (77 at planning time) |
| Lint | `cargo clippy -p codex-voice-tts --all-targets -- -D warnings` | exit 0 |
| Full gates | the four AGENTS.md commands | exit 0 |

## Scope

**In scope**:
- `crates/codex-voice-tts/src/codex_llm.rs`
- `crates/codex-voice-tts/Cargo.toml` (only if the core dependency is missing)

**Out of scope** (do NOT touch):
- `crates/codex-voice-core/src/fs.rs` — use it, don't modify it.
- `crates/codex-voice-codex/` — its auth path is separate (plan 002).
- The token-refresh HTTP logic (`refresh_tokens`) and expiry heuristics.

## Git workflow

- Branch: `advisor/003-harden-auth-json-io`
- One commit, e.g. `Harden speech-prep auth.json rewrite: atomic 0600 write, unique tmp, spawn_blocking`.

## Steps

### Step 1: Replace the unsafe write with `write_private_file_atomic` + unique tmp name

In `write_auth_file`:

1. Build a unique tmp path: keep the current `.{filename}.{pid}` prefix but append a random component, e.g. `format!(".{}.{}.{:08x}.tmp", filename, std::process::id(), rand::random::<u32>())`. `rand` is already a workspace dependency (`rand = "0.9.4"` in the root `Cargo.toml`); add `rand.workspace = true` to `crates/codex-voice-tts/Cargo.toml` if it isn't already a dependency of this crate.
2. Replace the `std::fs::write` + `set_permissions` + `rename` sequence with a single call to `codex_voice_core::fs::write_private_file_atomic(path, &tmp_path, format!("{text}\n").as_bytes())`, mapping the `std::io::Error` to `SpeechError::Auth(format!("failed to write refreshed Codex auth file: {error}"))`.
3. Keep `create_dir_all` for the parent directory.

**Verify**: `cargo check -p codex-voice-tts` → exit 0. `grep -n "set_permissions" crates/codex-voice-tts/src/codex_llm.rs` → no matches.

### Step 2: Move auth-file I/O off the async runtime

In `tokens()` (line 133):

- Wrap the `read_auth_file(&self.auth_file)` call in `tokio::task::spawn_blocking`, e.g.:

```rust
let auth_file = self.auth_file.clone();
let payload = tokio::task::spawn_blocking(move || read_auth_file(&auth_file))
    .await
    .map_err(|error| SpeechError::Auth(format!("auth read task failed: {error}")))??;
```

- Wrap the `write_auth_file(&self.auth_file, &refreshed)` call the same way (clone the path and move `refreshed` in, or pass a reference-counted value — `write_auth_file` takes `&Value`, so move the owned `Value` into the closure and pass `&refreshed` inside).
- `tokio` is already a dependency of this crate for the async client; confirm the `rt` feature surface compiles (the workspace tokio features include `rt-multi-thread`).

**Verify**: `cargo check -p codex-voice-tts` → exit 0.

### Step 3: Add a concurrency regression test

In the existing `mod tests` of `codex_llm.rs` (or create one following the pattern of other tts modules, e.g. `crates/codex-voice-tts/src/sanitize.rs` tests):

- `concurrent_auth_writes_do_not_corrupt_file`: in a `#[tokio::test(flavor = "multi_thread")]`, create a `tempfile::tempdir()`, target path `dir/auth.json`. Spawn 8 tasks each calling `write_auth_file` (or the new spawn_blocking wrapper) with a distinct valid JSON payload (e.g. `{"tokens":{"account_id":"task-N"}}`). Await all. Assert the final file parses as valid JSON via `serde_json::from_str::<serde_json::Value>` and equals one of the 8 payloads exactly (rename atomicity: last-writer-wins is fine, torn content is not).
- `written_auth_file_is_owner_only` (`#[cfg(unix)]`): after one write, assert `metadata.permissions().mode() & 0o777 == 0o600`.

Note: `write_auth_file` is a private free fn — the tests live in the same file's `mod tests`, so visibility is fine.

**Verify**: `cargo test -p codex-voice-tts codex_llm` → new tests pass.

### Step 4: Full workspace gates

**Verify**: `cargo fmt --check && cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → all exit 0.

## Test plan

Step 3's two tests: concurrent-write integrity and 0600 permissions. Model the temp-dir usage on existing tests in `crates/codex-voice-transcriber/src/server.rs` (`tempfile::tempdir()` usage around line 5882) — same crate-style, no network, no `$HOME`.

## Done criteria

- [ ] `write_auth_file` uses `write_private_file_atomic` with a randomized tmp path; no `set_permissions` call remains in `codex_llm.rs`
- [ ] `read_auth_file`/`write_auth_file` are invoked via `spawn_blocking` from `tokens()`
- [ ] 2 new tests pass; `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] Only in-scope files modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `write_private_file_atomic`'s signature differs from `(path, tmp_path, bytes) -> std::io::Result<()>` in a way that can't absorb this call shape.
- `write_private_file_atomic` fails on rename because the tmp file already exists from `create_new` semantics colliding with leftover tmp files — report rather than adding cleanup logic.
- The concurrency test reveals torn JSON even after the unique-tmp fix (would indicate rename is not atomic on the target filesystem — report, don't work around).

## Maintenance notes

- Two concurrent refreshes can still both hit the token endpoint (double network refresh) — harmless but wasteful; a `tokio::sync::Mutex` around `tokens()`'s refresh branch is a possible follow-up, deliberately deferred to keep this plan mechanical.
- If the Codex CLI ever changes auth.json's schema, `read_auth_file`/`extract_tokens` are the touch points; the write path is schema-agnostic.
- Reviewer should scrutinize: the double-`?` on the `spawn_blocking` join+inner result, and that `refreshed` is still available for `extract_tokens(&refreshed)` after being moved into the write closure (restructure: extract tokens BEFORE the write, or clone).
