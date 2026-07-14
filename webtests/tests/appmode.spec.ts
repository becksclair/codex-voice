import { test, expect } from '@playwright/test';
import fs from 'node:fs/promises';
import path from 'node:path';

// URL contract for Tauri app-mode webviews (see web/src/lib/appMode.ts):
// `?app=1` (app mode, not exercised here — see the SW specs in web.spec.ts for
// its one observable effect), `?view=settings` (settings-only window), and
// `#intent=<id>` (one-shot selected-text handoff + auto-generate).

const discoveryPath = path.resolve(__dirname, '../../target/webtests-state/codex-voice/transcriber.json');

// The billed-call firewall below relies on page.route, which cannot intercept
// requests issued through a service worker. SW behavior is covered by
// web.spec.ts; block SWs here so the firewall guarantee holds unconditionally.
test.use({ serviceWorkers: 'block' });

test.beforeEach(async ({ page }) => {
  await page.goto('/web');
  await page.evaluate(() => localStorage.clear());
});

test('#intent= consumes selected text, clears the hash, and fires a generation attempt', async ({
  page,
  request,
}) => {
  const sample = `desktop speak intake ${Date.now()} 🎙️ — héllo`;
  const discovery = JSON.parse(await fs.readFile(discoveryPath, 'utf8')) as { token: string };
  const created = await request.post('/web/desktop-intents', {
    headers: { Authorization: `Bearer ${discovery.token}` },
    data: { text: sample },
  });
  expect(created.status()).toBe(201);
  const { id } = (await created.json()) as { id: string };

  // A host with real `~/.codex/read-aloud-defaults.json` credentials would
  // otherwise let this test place a real (billed) synthesis call — this repo's
  // paid live smoke is deliberately opt-in (see live.spec.ts). Block every
  // request that isn't page-origin static content, INCLUDING same-origin
  // `/web/speech-jobs` (the server relays that to a real provider too), so
  // the generation attempt is observed but can never actually reach a
  // provider, regardless of what TTS config this host happens to have.
  let generationAttempted = false;
  await page.route('**/*', (route) => {
    const url = new URL(route.request().url());
    const isLocal = url.hostname === '127.0.0.1' || url.hostname === 'localhost';
    if (isLocal && !url.pathname.startsWith('/web/speech-jobs')) return route.continue();
    generationAttempted = true;
    return route.abort();
  });

  await page.goto(`/web#intent=${id}`);

  await expect(page.locator('#text')).toHaveValue(sample);
  await expect
    .poll(() => page.evaluate(() => location.hash))
    .toBe('');
  await expect.poll(() => generationAttempted).toBe(true);

  // The auto-generate fired for real (found the intake text, not the
  // empty-text guard) and — with every provider-bound call blocked above —
  // settled into the error banner rather than a real synthesis result.
  await expect(page.locator('#error-banner')).toBeVisible({ timeout: 10_000 });
  const message = (await page.locator('#error-banner').textContent())?.trim() ?? '';
  expect(message).not.toBe('');
  expect(message).not.toBe('Enter some text first.');
});

test('?view=settings opens the settings drawer on load', async ({ page }) => {
  await page.goto('/web?view=settings');

  await expect(page.locator('#settings-panel')).toBeVisible();
  await expect(page.locator('#text')).toHaveCount(0);
  await expect(page.locator('#generate')).toHaveCount(0);
});

test('consecutive button and native pastes generate the newly pasted text', async ({ page }) => {
  const generated: string[] = [];
  page.on('request', (request) => {
    if (
      new URL(request.url()).pathname === '/web/speech-jobs' &&
      request.method() === 'POST'
    ) {
      generated.push((request.postDataJSON() as { input: string }).input);
    }
  });
  await page.route('**/*', async (route) => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === '/web/config') {
      return route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
    }
    if (pathname.startsWith('/web/speech-jobs') && route.request().method() === 'POST') {
      return route.abort();
    }
    return route.continue();
  });
  await page.evaluate(() => localStorage.clear());
  await page.goto('/web?paste-regression=1');

  for (const value of ['first pasted draft', 'second pasted draft']) {
    await page.evaluate((text) => navigator.clipboard.writeText(text), value);
    await page.locator('#paste').click();
    await expect(page.locator('#text')).toHaveValue(value);
    await expect.poll(() => generated.at(-1)).toBe(value);
  }

  for (const value of ['first native paste', 'second native paste']) {
    await page.evaluate((text) => navigator.clipboard.writeText(text), value);
    await page.locator('#text').click();
    await page.locator('#text').press(process.platform === 'darwin' ? 'Meta+A' : 'Control+A');
    await page.locator('#text').press(process.platform === 'darwin' ? 'Meta+V' : 'Control+V');
    await expect(page.locator('#text')).toHaveValue(value);
    await expect.poll(() => generated.at(-1)).toBe(value);
  }

  expect(generated).toEqual([
    'first pasted draft',
    'second pasted draft',
    'first native paste',
    'second native paste',
  ]);
});

test('closing a stale settings window cannot overwrite the main draft', async ({ context, page }) => {
  await page.goto('/web');
  await page.locator('#text').fill('older draft');

  const settings = await context.newPage();
  await settings.goto('/web?view=settings');
  await expect(settings.locator('#settings-panel')).toBeVisible();

  await page.locator('#text').fill('newer main-window draft');
  await settings.close();
  await expect(page.locator('#text')).toHaveValue('newer main-window draft');
  await expect
    .poll(() => page.evaluate(() => localStorage.getItem('codex-voice.web.text')))
    .toBe('newer main-window draft');
});
