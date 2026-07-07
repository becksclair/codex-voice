# Codex Voice — standalone web frontend

Standalone React frontend for the Codex Voice TTS PWA. This app is built with Vite
and served in production under the `/web/` base path by the Rust transcriber
service (`crates/codex-voice-transcriber`). It will replace the embedded
single-file PWA at `crates/codex-voice-transcriber/assets/web/app.html`.

This directory is currently a **scaffold** (Phase A1): tooling, PWA/service-worker
wiring, theming tokens, and a placeholder page. The actual app port lands in a
later phase.

## Stack

- **Vite 8.1.x** (Rolldown-powered), `base: '/web/'`.
- **React 19.2.x** + react-dom, with the **React Compiler** enabled.
- **TypeScript 7 (native `tsc`)** via the `typescript@rc` tag — `tsc --noEmit`
  runs in `check` and `build`.
- **`@vitejs/plugin-react` v6** (oxc-based).
- **Tailwind CSS 4.3.x** with `@tailwindcss/vite`, CSS-first config in
  `src/index.css` (`@import "tailwindcss"` + `@theme`). Theme tokens: dark bg
  `#17091f`, light bg `#f3dff1`.
- **oxlint** (`.oxlintrc.json`: react plugin, correctness category, rules-of-hooks
  and exhaustive-deps as errors) and **oxfmt** as the only formatter
  (`.oxfmtrc.json`).
- **vitest** (happy-dom environment) for unit tests.
- **vite-plugin-pwa** (`generateSW`, `registerType: 'autoUpdate'`).

### Vite 8 vs 7 decision

**Vite 8** is used. `vite-plugin-pwa@1.3.0` declares
`"vite": "^3.1.0 || ^4.0.0 || ^5.0.0 || ^6.0.0 || ^7.0.0 || ^8.0.0"` in its
peerDependencies, so Vite 8 is officially supported. No pin to Vite 7 was needed.

### React Compiler status

**Enabled.** Wired through `@vitejs/plugin-react`'s `reactCompilerPreset` helper
plus `@rolldown/plugin-babel` and `babel-plugin-react-compiler@1`
(see `vite.config.ts`). The compiler runtime (`useMemoCache` / `_c(`) is present
in the production bundle, confirming it runs.

Note on `@babel/core`: it is pinned to `^7`, **not** `^8`. `workbox-build` (used by
vite-plugin-pwa to generate the service worker) requires `@babel/core@^7.0.0-0`,
and Bun hoists a single top-level copy. Installing `@babel/core@8` broke SW
generation with a Babel version-mismatch error; pinning to `^7` satisfies both
workbox and the React Compiler.

## Dev workflow

The one-command path from the repo root:

```bash
mise run dev    # audio server + Vite dev server with HMR; Ctrl-C stops both
```

Or run the Rust backend and the Vite dev server side by side:

```bash
# terminal 1 — backend (default 127.0.0.1:3845)
cargo run -p codex-voice-app --bin codex-voice -- server

# terminal 2 — frontend
cd web && bun run dev
```

The dev server proxies `/web/config`, `/web/speech`, and `/web/speech-jobs` to the
backend. Override the backend target with `CODEX_VOICE_BACKEND`:

```bash
CODEX_VOICE_BACKEND=http://127.0.0.1:9000 bun run dev
```

## Commands

- `bun run dev` — Vite dev server.
- `bun run build` — `tsc --noEmit` then `vite build`; outputs to `dist/`.
- `bun run check` — oxlint + `oxfmt --check` + `tsc --noEmit`.
- `bun run test` — vitest run.
- `bun run fmt` — oxfmt write.

From the repo root, the mise tasks wrap these: `dev` (full stack), `web-dev`,
`web-build`, `web-check`, `web-test`, and `web-fmt`, plus `serve` (backend only),
`test-web` (Playwright e2e), and `test-web-live` (paid live TTS smoke). See the
"Web Frontend" section of the root `AGENTS.md` for the full command table.

## PWA / manifests

- `manifest.webmanifest` (dark, `#17091f`) and `manifest-light.webmanifest`
  (light, `#f3dff1`) are shipped as static files in `public/`. The pre-paint
  script in `index.html` selects the active manifest and theme before first paint
  to avoid a flash. vite-plugin-pwa's own manifest generation is disabled
  (`manifest: false`) so there is exactly one `<link rel="manifest">`.
- The service worker is generated as `dist/sw.js` (plus a hashed
  `dist/workbox-*.js` runtime). `registerType: 'autoUpdate'`; registration is
  manual via `virtual:pwa-register` in `src/main.tsx`.
- `navigateFallback: '/web/index.html'` with
  `navigateFallbackDenylist: [/^\/web\/(config|speech)/]` and no runtime caching,
  so the JSON API surface (`/web/config`, `/web/speech`, `/web/speech-jobs`) is
  never served from cache or the SPA fallback.

## Route-shadowing constraint

The Rust service exposes JSON API routes under `/web/*`
(`GET /web/config`, `POST /web/speech`, `POST /web/speech-jobs`,
`GET /web/speech-jobs/{id}`). Because this app is served under the same `/web/`
base, **no file at the `dist/` root may be named `config`, `speech`, or
`speech-jobs`** — those paths are shadowed by the backend routes. Keep hashed
build assets under `dist/assets/`.
