# Web PWA behavioral tests

Playwright suite exercising the Codex Voice web PWA — the standalone React app
built from `web/` (see `web/README.md`) and served at `GET /web/` by
`codex-voice server` from the dist embedded in the binary.

These tests drive real browser behavior: persistence, clipboard handling,
responsive settings, cancellation/resume, service-worker routes, and the full
waveform/playback/seek/download path. Routine generation tests intercept the
speech-job API and return a deterministic in-memory WAV fixture, so they never
contact a provider or spend API credits. The web shell and its config endpoints
are intentionally unauthenticated, so no bearer token is required.

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

- Needs the operator's real `~/.config/codex-voice/config.json` on the host so
  `/web/config` returns live provider keys.
- **Cost per run:** one ~1.9k-character backend-first generation. The input is
  long enough to exercise backend prep and chunked synthesis. That is the
  entire default spend.
- The ElevenLabs leg is **off** even when `LIVE_TTS=1`; set
  `LIVE_TTS_ELEVENLABS=1` to add one short (~70-char) ElevenLabs synthesis.

In one browser session the Google leg asserts: config loads, provider select
populates, exactly one backend job is created, no browser-direct provider calls
occur, prepared text contains several bracketed cues, generation completes
(download/play enabled), no error banner, a plausible duration (> 10s), a
non-blank waveform canvas, playback advances, and the download is a valid WAV
(RIFF magic + > 100KB).

### Emotion-enrichment quality gate (paid, no audio)

`tests/enrichment-review.spec.ts` sends a public-domain passage from Mary
Shelley's *Frankenstein* through the real browser HTTP → backend Codex-prep
path using the configured model and reasoning effort. It calls the strict
prep-only endpoint and never invokes synthesis or generates audio:

```sh
mise run test-web-enrichment
```

The quality gate requires Luna with reasoning `none`, exact wording
preservation, 8–13 tags per 1,000 source characters, at least 75% unique tag
vocabulary, no single tag occupying more than 25% of insertions, at least two
tags in every third, a maximum 400-character cue gap (the fixture's final
sentence is long and semicolon-linked), clean insertion boundaries, and
coverage of at least four of five fixture-specific emotional beats. It also
rejects a small set of clearly contradictory emotions. The
complete enriched text and placement report is written to
`benchmark-results/enrichment-benchmark-configured-quality.md`.

For exploratory model comparison without richness thresholds, run:

```sh
mise run test-web-enrichment-benchmark
```

That task compares Luna and Terra by default. `ENRICHMENT_MODELS` can supply a
different comma-separated set, and the report includes latency, tag count,
unique tags, normalized positions, thirds distribution, maximum untagged gap,
and context around every insertion.

## Notes

- The server uses the platform Codex Voice config path
  (`~/.config/codex-voice/config.json` on Linux); it only *enables* TTS when present.
  Routine generation coverage intercepts `/web/config` and `/web/speech-jobs`,
  so a missing or real config file does not affect results.
- Clipboard access works because `127.0.0.1` is a secure context and the
  Playwright config grants `clipboard-read`/`clipboard-write` permissions.
- This suite is intentionally NOT wired into CI yet: downloading a browser is
  heavy. Enabling it in CI is a follow-up decision.
