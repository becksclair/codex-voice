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

## Live TTS smoke (paid, opt-in)

`tests/live.spec.ts` is a single-run, end-to-end smoke against the *real*
synthesis stack. It is skipped by default (both in the normal suite and when run
directly) and only executes when `LIVE_TTS=1` is set. It also skips cleanly when
`/web/config` returns `503` or exposes no providers (i.e. the host has no real
TTS config).

Run it via mise (builds the frontend first):

```sh
mise run test-web-live
# include the ElevenLabs leg (billed separately):
LIVE_TTS_ELEVENLABS=1 mise run test-web-live
```

Or directly:

```sh
cd webtests
LIVE_TTS=1 bunx playwright test tests/live.spec.ts
```

Requirements and cost:

- Needs the operator's real `~/.codex/read-aloud-defaults.json` on the host so
  `/web/config` returns live provider keys.
- **Cost per run:** one ~1.9k-character Google synthesis (crafted to cross the
  1600-char chunking threshold and split into three chunks, exercising the
  chunk/stitch path) plus one short (~20-char) server-job synthesis. That is the
  entire default spend.
- The ElevenLabs leg is **off** even when `LIVE_TTS=1`; set
  `LIVE_TTS_ELEVENLABS=1` to add one short (~70-char) ElevenLabs synthesis.

In one browser session the Google leg asserts: config loads, provider select
populates, generation completes (download/play enabled), no error banner, a
plausible duration (> 10s), a non-blank waveform canvas, playback advancing, and
a valid WAV download (RIFF magic + > 100KB). It then drives the server path
(`POST /web/speech-jobs` → poll to `complete` → decode base64 audio).

## Notes

- The server has no flag to override the TTS defaults path
  (`~/.codex/read-aloud-defaults.json`); it only *enables* TTS when present.
  These tests never trigger generation, so a missing/real config file does not
  affect results.
- Clipboard access works because `127.0.0.1` is a secure context and the
  Playwright config grants `clipboard-read`/`clipboard-write` permissions.
- This suite is intentionally NOT wired into CI yet: downloading a browser is
  heavy. Enabling it in CI is a follow-up decision.
