import { test, expect, type Page } from '@playwright/test';
import fs from 'node:fs/promises';
import path from 'node:path';

// URL contract for Tauri app-mode webviews (see web/src/lib/appMode.ts):
// `?app=1` (app mode), `?view=settings` (settings-only window), and
// `#intent=<id>` (one-shot selected-text handoff + auto-generate).

const discoveryPath = path.resolve(__dirname, '../../target/webtests-state/codex-voice/transcriber.json');

const composerSourceValue = (page: Page) =>
  page.evaluate(
    () =>
      (document.querySelector('[data-testid="composer-source"]') as HTMLTextAreaElement | null)
        ?.value ?? null,
  );

// Block service workers left over in a reused browser profile so the
// billed-call firewall below remains unconditional.
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

  // A host with real `~/.config/codex-voice/config.json` provider configuration would
  // otherwise let this test place a real (billed) synthesis call. Install a
  // deterministic local config response before navigating to the intent page, and
  // abort the provider-bound speech-job request; this keeps the test independent of
  // the host config while still allowing the local page assets to load normally.
  let generationAttempted = false;
  await page.route('**/*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const pathname = url.pathname;
    if (pathname === '/web/config') {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          version: 2,
          defaultProvider: 'google',
          providers: { google: { voice: 'Sulafat', models: ['test-model'] } },
          personas: {},
        }),
      });
    }
    if (pathname.startsWith('/web/speech-jobs')) {
      generationAttempted = true;
      return route.abort();
    }
    if (url.hostname !== '127.0.0.1' && url.hostname !== 'localhost') {
      return route.abort();
    }
    return route.continue();
  });

  // Use a distinct URL so Playwright performs a document navigation after the
  // route stubs are installed; a hash-only transition would retain the prior app.
  await page.goto(`/web?intent-e2e=1#intent=${id}`);

  await expect(page.locator('[data-testid="composer-source"]')).toHaveValue(sample);
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
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          version: 2,
          defaultProvider: 'google',
          providers: { google: { voice: 'Sulafat', models: ['test-model'] } },
          personas: {},
        }),
      });
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
    await expect.poll(() => composerSourceValue(page)).toBe(value);
    await expect(page.locator('#text')).toHaveText(value);
    await expect.poll(() => generated.at(-1)).toBe(value);
  }

  for (const value of ['first native paste', 'second native paste']) {
    await page.evaluate((text) => navigator.clipboard.writeText(text), value);
    await page.locator('#text').click();
    await page.locator('#text').press(process.platform === 'darwin' ? 'Meta+A' : 'Control+A');
    await page.locator('#text').press(process.platform === 'darwin' ? 'Meta+V' : 'Control+V');
    await expect.poll(() => composerSourceValue(page)).toBe(value);
    await expect(page.locator('#text')).toHaveText(value);
    await expect.poll(() => generated.at(-1)).toBe(value);
  }

  expect(generated).toEqual([
    'first pasted draft',
    'second pasted draft',
    'first native paste',
    'second native paste',
  ]);
  await expect
    .poll(() => page.evaluate(() => localStorage.getItem('codex-voice.web.text')))
    .toBe('second native paste');
});

test('closing a stale settings window cannot overwrite the main draft', async ({ context, page }) => {
  await page.goto('/web');
  await page.locator('#text').fill('older draft');

  const settings = await context.newPage();
  await settings.goto('/web?view=settings');
  await expect(settings.locator('#settings-panel')).toBeVisible();

  await page.locator('#text').fill('newer main-window draft');
  await settings.close();
  await expect.poll(() => composerSourceValue(page)).toBe('newer main-window draft');
  await expect
    .poll(() => page.evaluate(() => localStorage.getItem('codex-voice.web.text')))
    .toBe('newer main-window draft');
});
