import { defineConfig } from "vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";
import { VitePWA } from "vite-plugin-pwa";

const backend = process.env.CODEX_VOICE_BACKEND ?? "http://127.0.0.1:3845";

const proxyTargets = ["/web/config", "/web/speech", "/web/speech-jobs"];
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
      // Both manifests are shipped as static files in public/ (dark:
      // manifest.webmanifest, light: manifest-light.webmanifest) and swapped at
      // runtime by the pre-paint script in index.html. Disabling the plugin's
      // own manifest generation avoids a duplicate <link rel="manifest">.
      manifest: false,
      workbox: {
        navigateFallback: "/web/index.html",
        navigateFallbackDenylist: [/^\/web\/(config|speech)/],
        // Never cache the JSON API surface served by the Rust backend.
        runtimeCaching: [],
      },
    }),
  ],
  server: {
    proxy,
  },
});
