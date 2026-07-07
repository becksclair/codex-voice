# Web PWA behavioral tests

Playwright suite exercising the Codex Voice web PWA — the standalone React app
built from `web/` (see `web/README.md`) and served at `GET /web/` by
`codex-voice server` from the dist embedded in the binary.

These tests drive real browser behavior (localStorage persistence, character
counting, clipboard paste focus handling, manifest/service-worker routes). The
web shell and its config endpoints are intentionally unauthenticated, so no
bearer token is required. Tests deliberately avoid TTS generation, which is
disabled unless `~/.codex/read-aloud-defaults.json` exists.

## Running

`mise run test-web` builds the web frontend first (`web-build`) so the server
embeds the real React app rather than the stub page, then runs the suite. When
invoking Playwright directly, build the frontend yourself with
`mise run web-build` (or `cd web && bun run build`) before running the tests.

The Playwright `webServer` config compiles and launches the server binary
automatically, but the first `cargo` build is slow. Prebuild it first so the run
does not block past the server startup timeout:

```sh
# From the repo root:
mise run web-build
cargo build -p codex-voice-app

cd webtests
bun install
bunx playwright install chromium
bunx playwright test
```

The server is spawned on `127.0.0.1:38455` (a dedicated port that avoids the
default `3845`), with `reuseExistingServer: false`, and torn down after the run.

Via mise, from the repo root:

```sh
mise run test-web
```

## Notes

- The server has no flag to override the TTS defaults path
  (`~/.codex/read-aloud-defaults.json`); it only *enables* TTS when present.
  These tests never trigger generation, so a missing/real config file does not
  affect results.
- Clipboard access works because `127.0.0.1` is a secure context and the
  Playwright config grants `clipboard-read`/`clipboard-write` permissions.
- This suite is intentionally NOT wired into CI yet: downloading a browser is
  heavy. Enabling it in CI is a follow-up decision.
