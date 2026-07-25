# Web frontend performance

The Codex Voice web app is a React 19 client built with Vite 8,
the React Compiler, and Tailwind 4. `bun run build` reports the production
assets and enforces the initial-load budget.

## Current shape

- The browser loads one content-hashed application module and one stylesheet.
- Speech prep and fallback job admission run in Rust. ElevenLabs v3 MP3 streams
  directly to browser playback; Google Gemini 3.1 native PCM-over-SSE streams
  through a thin same-origin relay and is scheduled with Web Audio. Unsupported
  models use the server-job client.
- There is no service worker, Workbox runtime, offline precache, browser-direct
  cached configuration, pending-job recovery, or IndexedDB
  audio restore.
- `/web/config` is fetched from the backend with `cache: no-store`. If it is
  unavailable, generation stays disabled.
- Draft text and ordinary settings are the only durable browser state.

## Caching and startup

The HTML shell, manifests, and icons are served `no-cache`. Only
content-hashed `/web/assets/*` files receive immutable caching, so a navigation
revalidates the shell and naturally picks up new asset hashes.

The pre-paint theme bootstrap remains inline in `index.html` to avoid a theme
flash. No provider or audio work occurs during startup.

## Budget enforcement

`scripts/check-bundle-size.mjs`, run by `bun run build`, parses
`dist/index.html`, totals the gzip size of initial module scripts and
modulepreloads, and fails above 80,000 bytes gzip. The budget protects the
interactive shell from accidental dependency growth; provider functionality
belongs on the backend rather than in a deferred browser bundle.

## Runtime re-render sanity

The prompt textarea is controlled, so each keystroke re-renders `App`.
`WaveformPlayer` and `SettingsPanel` receive no text-derived props, and the
React Compiler handles memoization without hand-written `useMemo`,
`useCallback`, or `memo`.
