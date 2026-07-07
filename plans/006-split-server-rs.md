# Plan 006: Split the 6,701-line server.rs — extract web assets to files, modularize handlers, prune string-literal tests

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-transcriber/src/server.rs`
> This file is under active development (the working tree already differs from
> `701ed3f` at planning time: one removed `text.focus();` line in the paste
> handler plus a matching test assertion). Re-locate every line number in this
> plan with the greps provided rather than trusting offsets. On structural
> mismatch (a named constant or function missing), STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW (mechanical moves, no logic change)
- **Depends on**: plans/001-ci-verification-gates.md (recommended, not blocking)
- **Category**: tech-debt
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

`server.rs` is 6,701 lines — ~33× the repo's median file — because it contains four unrelated things: a ~3,820-line embedded web app (HTML/CSS/JS as one Rust string literal), a ~90-line service-worker JS string, ~1,100 lines of axum routing/handlers, and a ~1,390-line test module. The embedded frontend gets no syntax highlighting, no JS/CSS tooling, and no sane diffs; every server change churns a giant file. Additionally, ~200 of the test assertions are `.contains()` checks against CSS/pixel literals in the HTML string — they break on any styling tweak while catching nothing a browser would notice. This split is the enabling move for all future web-app work.

## Current state

- `crates/codex-voice-transcriber/src/server.rs` layout (verify each with the greps below):
  - `WEB_SW_BODY_JS` — service-worker JS string (line ~49).
  - `WEB_APPLE_TOUCH_ICON` etc. — PNG assets already externalized via `include_bytes!("../assets/web/apple-touch-icon.png")` (line ~44). **The `assets/web/` directory already exists** — this plan follows the established pattern.
  - `WEB_APP_HTML` — `r##"<!doctype html> ... "##` from line ~139 to ~3961.
  - Helper/browser-config fns from ~4222; `serve()` ~4460; `service_router` ~4521; handlers ~4566–5275 (`health`, `transcribe`, `web_app`, `web_config`, manifest/SW/icon handlers, `transcribe_upload/direct/chunked`, `web_speech*`, `speech`, `synthesize_response`, `authorize`, `constant_time_eq`, `shutdown_signal`).
  - `mod tests` from ~5311 to end. 330 `.contains()` assertions total; the bulk live in `web_app_returns_phone_tts_shell` (~line 5369) asserting CSS literals like `"height: 34px;"`, `"opacity: 0;"`, `"-webkit-tap-highlight-color: transparent;"`.
  - `web_app_body()` (~4611) does exactly 5 `.replace("__WEB_*_URL__", ...)` substitutions on `WEB_APP_HTML` — so `include_str!` is a drop-in.
- Locator greps (use these, not line numbers):

```bash
grep -n "const WEB_APP_HTML\|const WEB_SW_BODY_JS\|fn web_app_body\|fn service_router\|^mod tests\|fn web_app_returns_phone_tts_shell" crates/codex-voice-transcriber/src/server.rs
```

- Repo conventions: `AGENTS.md` (root) mandates: "If an asset route uses long-lived or `immutable` caching, every HTML, manifest, and service-worker precache reference must include the current build revision" — the existing `WEB_BUILD_REVISION` / `versioned_web_asset` / `web_cache_name` mechanism implements this. **Do not alter it.**
- Genuinely valuable tests to preserve untouched: the `tower::ServiceExt::oneshot` HTTP tests (auth rejection, CORS, 413 oversize, response-format negotiation, constant-time compare, hot config reload) — grep `oneshot(` to enumerate.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-transcriber` | exit 0 |
| Tests | `cargo test -p codex-voice-transcriber` | all pass (55 at planning; fewer after Step 4 prune — count the prune) |
| Lint | `cargo clippy -p codex-voice-transcriber --all-targets -- -D warnings` | exit 0 |
| Byte-identical asset check | see Step 1 verify | identical output |

## Scope

**In scope**:
- `crates/codex-voice-transcriber/src/server.rs`
- `crates/codex-voice-transcriber/src/server/` (new submodule directory, if chosen in Step 3)
- `crates/codex-voice-transcriber/assets/web/app.html` (create)
- `crates/codex-voice-transcriber/assets/web/sw.js` (create)
- `crates/codex-voice-transcriber/src/lib.rs` (module declarations only)

**Out of scope** (do NOT touch):
- Any route path, handler signature, response header, or the `WEB_BUILD_REVISION` cache-busting scheme — zero behavior change.
- `chunking.rs`, `upload.rs`, `client.rs`, `discovery.rs`, `test_support.rs`.
- The HTML/JS content itself — no reformatting, no "improvements" to the web app; the extraction must be byte-preserving.

## Git workflow

- Branch: `advisor/006-split-server-rs`
- Commit per step (extraction, handler split, test move, test prune) so each is independently revertible. NOTE: the working tree may hold an uncommitted `server.rs` change — ask the operator to commit or stash it before starting; do not absorb unrelated changes into your commits.

## Steps

### Step 1: Extract the two frontend strings to asset files

1. Copy the exact contents of the `WEB_APP_HTML` raw string (everything between `r##"` and `"##`) into `crates/codex-voice-transcriber/assets/web/app.html`. Preserve bytes exactly — no editor autoformat, no trailing-newline addition beyond what the literal contains.
2. Same for `WEB_SW_BODY_JS` → `assets/web/sw.js`.
3. Replace the constants:

```rust
const WEB_APP_HTML: &str = include_str!("../assets/web/app.html");
const WEB_SW_BODY_JS: &str = include_str!("../assets/web/sw.js");
```

(Adjust the relative path to match the actual location of the declaring file — from `src/server.rs` it is `../assets/web/app.html`, matching the existing `include_bytes!` pattern at the top of the file.)

**Verify** (byte-identical extraction): `cargo test -p codex-voice-transcriber` → all 55 tests pass unchanged. The remaining `.contains()` tests act as extraction checksums here — that is why this step precedes the prune. Also `cargo check -p codex-voice-transcriber` → exit 0.

### Step 2: Extract with a script, not by hand

(Belongs to Step 1 — guidance): the string is ~3,800 lines; do the copy mechanically, e.g.:

```bash
awk '/^const WEB_APP_HTML: &str = r##"/{flag=1; sub(/^const WEB_APP_HTML: &str = r##"/,""); } flag && /^"##;$/{flag=0} flag' ...
```

or open the file, locate the literal's first/last lines by grep, and use `sed -n 'START,ENDp'`. After writing the asset file, verify no `r##`-delimiter residue remains in it: `grep -c '"##' assets/web/app.html` → 0.

### Step 3: Split the Rust code into submodules

Convert `server.rs` into a directory module (keep the public API of the crate identical — `lib.rs` re-exports must not change):

- `src/server.rs` → `src/server/mod.rs`: `ServiceState`, `ServiceAuth`, `serve()`, `service_router`, `shutdown_signal`, `authorize`, `constant_time_eq`, `ApiError`, and the shared types. Re-export what the submodules need.
- `src/server/web.rs`: the asset constants, `web_app`, `web_app_body`, `web_config`, `web_build_version`, `web_cache_name`, `versioned_web_asset`, `web_manifest*`, `web_service_worker*`, `web_png_response`, icon handlers, browser-config builder fns (`browser_*`), `WebSpeechJob*` types, `web_speech*` handlers, `prune_web_speech_jobs`, `synthesize_web_speech`, `web_speech_job_id`.
- `src/server/speech.rs`: `speech`, `synthesize_response`, `web_speech_client`, TTS state/reload (`watch_tts_config`, `config_fingerprint`, `reload_tts_config_once`, `TtsServiceState`).
- `src/server/transcribe.rs`: `transcribe`, `transcribe_upload`, `transcribe_direct`, `transcribe_chunked`.
- Tests: move `mod tests` into `src/server/tests.rs` (declared `#[cfg(test)] mod tests;` in `mod.rs`). Splitting tests per submodule is optional; a single `tests.rs` is fine.

Visibility: prefer `pub(crate)`/`pub(super)` over `pub`. This is a mechanical move — if a function seems to need reworking to move, it goes in `mod.rs` instead; do not redesign.

**Verify**: `cargo check -p codex-voice-transcriber && cargo test -p codex-voice-transcriber` → exit 0, all tests pass. `wc -l crates/codex-voice-transcriber/src/server/*.rs` → no file over ~1,500 lines (tests.rs may be larger until Step 4).

### Step 4: Prune the string-literal assertions

In the moved tests: `web_app_returns_phone_tts_shell` and siblings assert dozens of CSS/pixel/attribute literals. Reduce to a behavioral core:

**Keep** (behavior/serving contract):
- Response status 200 + `text/html` content type for `GET /web`.
- All five `__WEB_*_URL__` placeholders are substituted (assert `!body.contains("__WEB_")`).
- Presence of DOM element IDs that the embedded JS binds to (`grep -o 'getElementById([^)]*)' assets/web/app.html | sort -u` gives the authoritative list — assert each ID exists as `id="..."` in the served body).
- Manifest/SW routes: correct content types and build-revision versioning (existing tests — keep).
- All `oneshot` HTTP tests (auth/CORS/413/format/config-reload) — keep untouched.

**Delete**: every assertion on CSS property values, pixel literals, meta theme-color hexes, JS source fragments (e.g. `contains("const valueBeforePaste")`, `contains("height: 34px;")`). Rationale for the executor: these assert that a string contains substrings of itself; they are change-detectors, not tests.

**Verify**: `cargo test -p codex-voice-transcriber` → all remaining tests pass. `grep -c '\.contains("' crates/codex-voice-transcriber/src/server/tests.rs` → expect roughly ≤80 (from 330); record the exact number in your report.

### Step 5: Full gates

**Verify**: `cargo fmt --check && cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → all exit 0.

## Test plan

No new tests. The invariant is preservation: Step 1 keeps all 55 tests green as an extraction checksum; Step 4 then deletes only zero-signal assertions (documented above) plus adds the placeholder-substitution and getElementById-coverage assertions if not already present.

## Done criteria

- [ ] `assets/web/app.html` and `assets/web/sw.js` exist; the Rust constants are `include_str!`
- [ ] `src/server/` contains `mod.rs`, `web.rs`, `speech.rs`, `transcribe.rs`, `tests.rs`; no source file >1,500 lines
- [ ] Route list in `service_router` unchanged (diff shows moves only)
- [ ] `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Any test fails after Step 1 — the extraction was not byte-identical; diff the asset file against the original literal, do not "fix" tests.
- A handler cannot move without changing its signature or a route's behavior.
- The uncommitted working-tree change conflicts with your edits and the operator is unavailable to resolve it.
- `include_str!` path resolution fails in a way that suggests the crate layout differs from this plan's assumption.

## Maintenance notes

- Future web-app edits now happen in `assets/web/app.html` with real HTML/JS tooling. A follow-up (deliberately deferred) could add JS-level tests (Playwright against `/web`) — see plans/README.md deferred list.
- Plan 007 (web shell caching/compression) edits `web_app_body`/router — land 006 first; 007's excerpts assume the post-split layout names.
- The `AGENTS.md` cache-busting rule still applies: any new asset reference in the HTML must go through `versioned_web_asset`.
- Reviewer should scrutinize: that the diff is move-only for handlers (no logic edits), and the deleted-assertion list against the keep-list above.
