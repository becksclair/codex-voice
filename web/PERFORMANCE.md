# Web frontend performance

Measurement-driven performance notes for the Codex Voice web app (Vite 8 +
React 19 with the React Compiler + Tailwind 4, served from the Rust
`codex-voice-app` embed over Tailscale). Numbers below are from `bun run build`
(gzip figures are Vite's report unless stated otherwise) and headless Lighthouse
against `bun run preview`.

## Bundle sizes: before vs after

Baseline is the state at the start of Phase E (single monolithic entry chunk).
"After" is the shipped state with the generation pipeline code-split behind a
dynamic `import()`.

| Asset                                   | Before (raw / gzip)      | After (raw / gzip)       |
| --------------------------------------- | ------------------------ | ------------------------ |
| `index.html`                            | 1.96 kB / 0.80 kB        | 1.96 kB / 0.80 kB        |
| `assets/index-*.css`                    | 25.48 kB / 6.23 kB       | 25.48 kB / 6.23 kB       |
| `assets/workbox-window.*.js` (deferred) | 5.65 kB / 2.20 kB        | 5.65 kB / 2.20 kB        |
| `assets/index-*.js` (entry)             | 275.69 kB / **87.34 kB** | 230.56 kB / **73.39 kB** |
| `assets/generation-*.js` (lazy)         | —                        | 45.59 kB / 14.86 kB      |
| PWA precache                            | 4 entries / 301.55 KiB   | 5 entries / 302.00 KiB   |

**Initial-load JS (the entry chunk) dropped 87.34 kB → 73.39 kB gzip, about
16%.** Measured with zlib level 9 (the budget guard's method) the entry is
72,678 bytes gzip. The generation pipeline (14.86 kB gzip) now loads on the
first generate instead of at boot, and is included in the service-worker
precache so it is available offline after the first visit.

### What is in the entry vs the lazy chunk

The single largest cost in the entry is the React runtime
(`react-dom` + `react` + `scheduler`), which is the irreducible framework floor.
The code split moves the entire speech-generation pipeline — reachable only when
the user actually generates — into the lazy `generation` chunk:

- `lib/generation.ts` — the generation controller (run lifecycle, provider
  selection, fallback, persistence).
- `lib/prep/**` — speech-prep, including the Codex SSE client, tag handling, and
  prompt construction (the heaviest cluster: `tags.ts`, `prepare.ts`,
  `codex.ts`, `prompts.ts`, `decision.ts`).
- `lib/synth/google.ts`, `lib/synth/elevenlabs.ts`, `lib/synth/serverJobs.ts`,
  and their chunking/pool/common helpers — direct-provider synthesis.
- `lib/audio/streaming.ts` — the streaming playback engine (the single largest
  source module) and `lib/audio/wav.ts` — used only on the synthesis path.

The app shell keeps only what first paint and restored-audio playback need:
the editor, settings, waveform rendering (`lib/audio/waveform.ts`,
`waveform-controller.ts`), storage/config/theme, and the persona/provider
resolvers used by the settings panel (`lib/personas.ts`).

## Decisions taken vs skipped

- **Code-split the generation pipeline (taken).** `useGeneration` no longer
  statically imports the controller; it dynamically `import()`s
  `lib/generation.ts` on the first generate. To make the split effective, two
  import-boundary moves were required (no `lib/` semantics changed):
  - `shouldApplyGeneratedText` moved from `generation.ts` to `storage.ts` so the
    shell's restored-audio path can use it without pulling the pipeline.
  - The pure persona/provider resolvers (`personaSupportsProvider`,
    `firstPersonaForProvider`, `selectedPersonaName`, `resolvePersona`,
    `resolveProvider`) moved to a new lightweight `lib/personas.ts`. The settings
    panel needs these at load; leaving them in `generation.ts` kept the whole
    pipeline statically reachable. `generation.ts` re-exports them for API
    compatibility, and the shell barrel (`lib/index.ts`) exports `personas.ts`
    instead of `generation.ts`.
- **Defer controller construction (taken).** The controller was built on mount.
  It is now built lazily on first generate. The one case that must still work at
  load — resuming a persisted server job — is handled by eagerly running the
  dynamic import only when `loadPendingGeneration()` returns a record (at mount,
  and on `pageshow`/`visibilitychange`). `resumePending()` therefore still runs
  on load whenever there is pending work.
- **Tailwind purge (verified, no change needed).** Tailwind 4 tree-shakes unused
  utilities by default; the 25.48 kB CSS is the in-use design-token set plus the
  utilities the components reference. No legacy/dead CSS remains (the legacy
  `<style>` block was converted to utilities in an earlier phase).
- **Double-bundling / dead weight (verified clean).** The production build is 61
  modules: only `src/**` and React. No `@testing-library`, `fake-indexeddb`,
  `happy-dom`, or `vitest` leak into `dist`. No sourcemaps are emitted.
  `workbox-window` is a separate ~2.2 kB gzip chunk loaded by the service-worker
  registration (`virtual:pwa-register`), not part of first paint.
- **Further splitting the settings panel or waveform (skipped).** These are part
  of first paint / restored-audio playback and small relative to the framework
  floor; splitting them would add request round-trips for no meaningful initial
  payload win.

## Startup path

- `index.html` loads the entry via a single `<script type="module" crossorigin>`
  tag; the generation chunk is the only dynamic chunk, so no extra
  `modulepreload` links are needed for first paint.
- The pre-paint theme bootstrap stays inline in `<head>` so the theme is applied
  before first paint (no flash).
- The `/web/config` fetch remains cached-then-network (unchanged).
- IndexedDB restore of the last generated audio runs at effect time inside
  `useGeneration` (not during render), so it never blocks first paint.

## Budget enforcement

`scripts/check-bundle-size.mjs` (run via `bun run budget`, wired into
`bun run build`) parses `dist/index.html`, sums the gzip size of the entry
module script plus any `modulepreload` chunks (i.e. the JS the browser must
fetch for first paint), and fails the build if it exceeds the budget.

- **Budget: 80,000 bytes gzip.** Rationale: the post-split baseline is 72,678
  bytes gzip (zlib level 9); 80,000 is roughly +10% headroom, rounded. The lazy
  generation chunk is intentionally excluded — it is fetched on first generate,
  not at load. Current headroom: ~9.2% (~7.3 kB).

## Runtime re-render sanity

The prompt textarea is controlled, so each keystroke re-renders `App`. The
waveform (`WaveformPlayer`) and `SettingsPanel` receive no text-derived props,
and with the React Compiler enabled they are auto-memoized, so keystrokes do not
re-render them. Verified by inspection of the prop flow in `App.tsx`; no
re-render problem was found, so no regression test was added.

## Lighthouse

Headless Chrome against `bun run preview` (`http://localhost:4173/web/`).

| Preset             | Performance | Best-Practices | FCP   | LCP   | TBT  | CLS | Speed Index |
| ------------------ | ----------- | -------------- | ----- | ----- | ---- | --- | ----------- |
| Desktop            | 100         | 96             | 0.4 s | 0.4 s | 0 ms | 0   | 0.4 s       |
| Mobile (throttled) | 99          | 96             | 1.3 s | 2.0 s | 0 ms | 0   | 1.3 s       |

Re-run with:

```
bun run build
bun run preview --port 4173 &
CHROME_PATH=/usr/bin/google-chrome-stable bunx lighthouse \
  http://localhost:4173/web/ --only-categories=performance,best-practices \
  --chrome-flags="--headless --no-sandbox --disable-gpu" --quiet
```
