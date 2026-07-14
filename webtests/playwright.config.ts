import { defineConfig, devices } from '@playwright/test';
import path from 'node:path';

// The web shell and its config endpoints are deliberately unauthenticated, so
// no bearer token is needed. The server binds to a dedicated test port to avoid
// colliding with a developer's running instance on the default 3845.
const PORT = 38455;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// Repo root is one level up from this config file.
const repoRoot = path.resolve(__dirname, '..');
const testStateHome = path.join(repoRoot, 'target', 'webtests-state');

export default defineConfig({
  testDir: './tests',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: process.env.CI ? 'line' : [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: BASE_URL,
    trace: 'on-first-retry',
    // localhost is a secure context, so clipboard APIs are available once the
    // Chromium clipboard permissions are granted (see the paste test).
    permissions: ['clipboard-read', 'clipboard-write'],
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: {
    // Spawn the compiled server binary. Prebuild with
    // `cargo build -p codex-voice-app` so the first test run does not block on a
    // slow compile past the timeout below.
    command: `cargo run -q -p codex-voice-app --bin codex-voice -- server --bind 127.0.0.1:${PORT} --web-dist web/dist`,
    cwd: repoRoot,
    url: `${BASE_URL}/web`,
    reuseExistingServer: false,
    timeout: 180_000,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      ...process.env,
      XDG_STATE_HOME: testStateHome,
    },
  },
});
