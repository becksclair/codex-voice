import { defineConfig } from "vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";

const backend = process.env.CODEX_VOICE_BACKEND ?? "http://127.0.0.1:3845";

export const proxyTargets = [
  "/web/config",
  "/web/google-stream",
  "/web/speech",
  "/web/speech-jobs",
  "/web/desktop-intents",
];
const proxy = Object.fromEntries(
  proxyTargets.map((path) => [path, { target: backend, changeOrigin: true }]),
);

export default defineConfig({
  base: "/web/",
  plugins: [react(), babel({ presets: [reactCompilerPreset()] }), tailwindcss()],
  server: {
    proxy,
  },
});
