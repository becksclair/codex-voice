# Web frontend performance

The Codex Voice web app is a React 19 client built with Vite 8,
the React Compiler, and Tailwind 4. `bun run build` reports the production
assets and enforces the initial-load budget.

## Current shape

- The browser loads the content-hashed application shell and stylesheet first.
  The Lexical Markdown editor is a separate lazy chunk loaded after the shell
  becomes interactive, so it does not count against the 80 kB initial budget.
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

## Composer runtime

The Lexical document owns the live editing state. Markdown export and the React
draft update are coalesced across a 120 ms idle window, and localStorage writes
are separately debounced by 350 ms. Generation and paste-boundary reads use the
source mirror's synchronous `.value` getter, so they cannot observe a stale
export. This keeps full-document Markdown serialization off the per-keystroke
hot path while preserving the existing generation input contract.

Large documents still incur a full Markdown export after an editing burst and
when generation reads the source. The current acceptance target is a roughly
100 kB Markdown document; larger documents remain supported but are not covered
by a fixed responsiveness guarantee.
