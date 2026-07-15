import { defineConfig } from "vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";
import { VitePWA } from "vite-plugin-pwa";

const backend = process.env.CODEX_VOICE_BACKEND ?? "http://127.0.0.1:3845";

export const proxyTargets = [
  "/web/config",
  "/web/codex-auth",
  "/web/speech",
  "/web/speech-jobs",
  "/web/desktop-intents",
];
export const backendNavigationDenylist = [/^\/web\/(config|codex-auth|speech|desktop-intents)/];
export const pwaWorkboxOptions = {
  // Registration is manual, so vite-plugin-pwa does not inject these defaults
  // for `autoUpdate`. Without them, Android can keep a new worker waiting and
  // continue reopening the old precached shell indefinitely.
  skipWaiting: true,
  clientsClaim: true,
  // Default globs skip png/webmanifest, so the icons and both manifests were
  // absent from the precache and failed offline (the legacy SW cached them).
  globPatterns: ["**/*.{js,css,html,png,webmanifest}"],
  navigateFallback: "/web/index.html",
  navigateFallbackDenylist: backendNavigationDenylist,
  // Never cache the JSON API surface served by the Rust backend.
  runtimeCaching: [],
};
const proxy = Object.fromEntries(
  proxyTargets.map((path) => [path, { target: backend, changeOrigin: true }]),
);

export default defineConfig({
  base: "/web/",
  plugins: [
    react(),
    babel({ presets: [reactCompilerPreset()] }),
    tailwindcss(),
    VitePWA({
      registerType: "autoUpdate",
      strategies: "generateSW",
      // Registration is done manually in src/main.tsx via virtual:pwa-register.
      injectRegister: false,
      // The app's canonical URL is /web (no trailing slash), but a worker
      // script at /web/sw.js can only claim /web/ by default — which does NOT
      // cover /web itself, leaving the installed PWA uncontrolled and never
      // offline. Register with scope /web; the Rust server sends
      // Service-Worker-Allowed: /web on the sw.js response to authorize it.
      scope: "/web",
      // Both manifests are shipped as static files in public/ (dark:
      // manifest.webmanifest, light: manifest-light.webmanifest) and swapped at
      // runtime by the pre-paint script in index.html. Disabling the plugin's
      // own manifest generation avoids a duplicate <link rel="manifest">.
      manifest: false,
      workbox: pwaWorkboxOptions,
    }),
  ],
  server: {
    proxy,
  },
});
