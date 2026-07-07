# Plan 007: Memoize the web shell and serve it compressed with cache headers

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-transcriber/src crates/codex-voice-transcriber/Cargo.toml`
> If plan 006 has landed, the functions named here live in
> `src/server/web.rs` / `src/server/mod.rs` instead of `src/server.rs` —
> that is expected, not drift. Locate them by name with grep.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/006-split-server-rs.md (soft — the change is the same either way; file paths differ)
- **Category**: perf
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

`GET /web` rebuilds the ~259 KB web-app HTML on every request via five sequential full-string `.replace()` calls (~1.3 MB of allocation and scanning per hit) even though the output is a pure function of compile-time constants. The response is also served uncompressed and without cache headers, so every phone reload re-downloads a quarter-megabyte of highly compressible text. Memoize once, add a compression layer, and set a revalidation header.

## Current state

- `web_app_body()` — five `.replace("__WEB_*_URL__", &versioned_web_asset(...))` calls on `const WEB_APP_HTML`. All substituted values derive from `CARGO_PKG_VERSION` + `WEB_BUILD_REVISION` (compile-time), via `web_build_version()`.

```rust
// server.rs:4607-4633 at planning (post-006: src/server/web.rs)
async fn web_app() -> Html<String> {
    Html(web_app_body())
}
fn web_app_body() -> String {
    WEB_APP_HTML
        .replace("__WEB_MANIFEST_URL__", &versioned_web_asset("/web/manifest.webmanifest"))
        // ... 4 more .replace calls ...
}
```

- Router construction: `service_router(state)` builds the `Router` and applies `CorsLayer` (`.layer(cors)`); grep `fn service_router`. `tower-http` is already a workspace dependency but with only the `cors` feature: root `Cargo.toml` line 44: `tower-http = { version = "0.6.2", features = ["cors"] }`.
- Icon handlers already set `Cache-Control` headers (grep `CACHE_CONTROL` for the exemplar — `web_png_response` uses `immutable`); the HTML route sets none.
- The service worker + manifest use the `WEB_BUILD_REVISION` versioning scheme for cache busting (root `AGENTS.md` mandates this). The HTML shell itself is fetched by the SW/browser — it must NOT get `immutable` caching; `no-cache` (always revalidate) is the correct policy for the shell.
- Existing tests asserting on `/web` responses: grep `web_app_returns` in the transcriber test module.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-transcriber` | exit 0 |
| Tests | `cargo test -p codex-voice-transcriber` | all pass |
| Lint | `cargo clippy -p codex-voice-transcriber --all-targets -- -D warnings` | exit 0 |

## Scope

**In scope**:
- The file containing `web_app`/`web_app_body` and `service_router` (`src/server.rs`, or `src/server/{web,mod}.rs` post-006)
- Root `Cargo.toml` (add `compression-gzip`/`compression-br` features to the existing `tower-http` entry — feature addition only)
- The transcriber test module (new assertions)

**Out of scope** (do NOT touch):
- `WEB_BUILD_REVISION` / `versioned_web_asset` / `web_cache_name` — the cache-busting contract stays as-is.
- Icon/manifest/SW handlers' existing headers.
- The HTML content and all non-web routes.

## Git workflow

- Branch: `advisor/007-web-shell-caching`
- One commit, e.g. `Memoize web shell body; add compression and revalidation caching`.

## Steps

### Step 1: Memoize the shell body

```rust
fn web_app_body() -> &'static str {
    static BODY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BODY.get_or_init(|| {
        WEB_APP_HTML
            .replace(/* existing five replace calls, unchanged */)
    })
}
```

Update `web_app()` to `Html(web_app_body().to_string())` — or better, return `impl IntoResponse` with `([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], web_app_body())` serving the `&'static str` without a copy. Update any other `web_app_body()` callers (grep for them — the test module calls it; `&'static str` derefs fine in `.contains()` assertions).

**Verify**: `cargo test -p codex-voice-transcriber` → all pass.

### Step 2: Add `Cache-Control: no-cache` to the shell response

(Combined into Step 1's response tuple if you took that shape.) The expectation: `GET /web` response carries `cache-control: no-cache` so browsers revalidate but can reuse.

**Verify**: new/updated test (Step 4) asserts the header.

### Step 3: Add the compression layer

1. Root `Cargo.toml`: extend the tower-http entry to `features = ["cors", "compression-gzip", "compression-br"]`.
2. In `service_router`, after the CORS layer: `.layer(tower_http::compression::CompressionLayer::new())`.

Compression applies per `Accept-Encoding`; API JSON/audio responses are unaffected for clients that don't request it, and compressing audio responses is wasteful but harmless — if you want to exclude them, use `CompressionLayer::new().compress_when(...)` with a predicate on content type; otherwise the default is acceptable. Keep it simple: default layer.

**Verify**: `cargo check -p codex-voice-transcriber` → exit 0 (feature resolution worked).

### Step 4: Test the new response contract

Add to the transcriber tests (model on the existing `oneshot` tests, grep `oneshot(`):

1. `web_app_sets_no_cache_and_html_content_type` — `GET /web`, assert 200, `content-type` starts with `text/html`, `cache-control == "no-cache"`.
2. `web_app_serves_gzip_when_requested` — `GET /web` with `Accept-Encoding: gzip`, assert `content-encoding == "gzip"` and the body is smaller than the identity body length.

**Verify**: `cargo test -p codex-voice-transcriber` → all pass including 2 new.

### Step 5: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

Step 4's two tests. Existing `/web` content tests must pass unchanged (memoization is behavior-preserving).

## Done criteria

- [ ] `web_app_body` computes at most once per process (OnceLock)
- [ ] `GET /web` sends `Cache-Control: no-cache` and honors `Accept-Encoding: gzip`
- [ ] `cargo test --workspace` exits 0 with the 2 new tests
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Any substituted placeholder value turns out NOT to be compile-time constant (memoization would freeze a dynamic value — grep the five replace arguments' call chains first).
- The compression layer breaks a streaming/audio endpoint test.
- tower-http 0.6's compression feature names differ from `compression-gzip`/`compression-br` (check its docs if resolution fails).

## Maintenance notes

- If a future change makes the shell dynamic per-request (e.g. injecting per-user config), the OnceLock must be removed — the memoization is only valid while the body is constant. Leave a one-line comment on the OnceLock stating this invariant.
- The PWA service worker caches aggressively; `no-cache` on the shell plus the existing `WEB_BUILD_REVISION` scheme is the intended update path. Do not add `immutable` to the shell.
- Reviewer should scrutinize: that all `web_app_body()` call sites compile with the `&'static str` return type.
