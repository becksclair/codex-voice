import { test, expect } from '@playwright/test';

// Storage keys used by the PWA (see assets/web/app.html).
const TEXT_KEY = 'codex-voice.web.text';
const SETTINGS_KEY = 'codex-voice.web.settings.v1';

// Start every test from a clean slate so persisted localStorage from a prior
// test never leaks in and masks a real regression.
test.beforeEach(async ({ page }) => {
  await page.goto('/web');
  await page.evaluate(() => localStorage.clear());
  await page.reload();
});

test('shell loads with title, textarea, and generate button', async ({ page }) => {
  await expect(page).toHaveTitle('Codex Voice');
  await expect(page.locator('#text')).toBeVisible();
  await expect(page.locator('#generate')).toBeVisible();
});

test('typed text persists across a reload', async ({ page }) => {
  const sample = 'persistence check ' + Date.now();
  const textarea = page.locator('#text');
  await textarea.click();
  await textarea.fill(sample);

  // The input handler writes to localStorage synchronously; assert it landed
  // before reloading to avoid a race with the reload.
  await expect
    .poll(() => page.evaluate((k) => localStorage.getItem(k), TEXT_KEY))
    .toBe(sample);

  await page.reload();
  await expect(page.locator('#text')).toHaveValue(sample);
});

test('paste button fills the textarea without stealing focus', async ({ page }) => {
  // e2e version of the Rust regression guard
  // (web_paste_handler_does_not_refocus_textarea): the paste-button click must
  // populate the textarea but must NOT call text.focus(), which previously stole
  // focus from wherever the user had it.

  // Disable "generate on paste" so the paste flow does not attempt TTS
  // generation (TTS is disabled without ~/.codex/read-aloud-defaults.json).
  await page.locator('#settings-toggle').click();
  const generateOnPaste = page.locator('#generate-on-paste');
  await expect(generateOnPaste).toBeVisible();
  if (await generateOnPaste.isChecked()) {
    await generateOnPaste.uncheck();
  }
  await page.locator('#settings-toggle').click();

  const pasted = 'clipboard payload ' + Date.now();
  await page.evaluate((value) => navigator.clipboard.writeText(value), pasted);

  // Move focus onto a control that is NOT the textarea, then paste.
  const settingsToggle = page.locator('#settings-toggle');
  await settingsToggle.focus();
  await expect(settingsToggle).toBeFocused();

  await page.locator('#paste').click();

  // The paste flow populated the textarea...
  await expect(page.locator('#text')).toHaveValue(pasted);

  // ...but focus did NOT jump into the textarea (the regression being guarded).
  const textareaFocused = await page.evaluate(
    () => document.activeElement === document.getElementById('text'),
  );
  expect(textareaFocused).toBe(false);
});

test('character count updates as the user types', async ({ page }) => {
  const count = page.locator('#count');
  await expect(count).toHaveText('0 chars');

  const textarea = page.locator('#text');
  await textarea.click();
  await textarea.type('a');
  await expect(count).toHaveText('1 char');

  await textarea.type('bcd');
  await expect(count).toHaveText('4 chars');
});

test('manifest route returns JSON with the app name', async ({ request }) => {
  const res = await request.get('/web/manifest.webmanifest');
  expect(res.status()).toBe(200);
  expect(res.headers()['content-type']).toContain('manifest+json');
  const manifest = await res.json();
  expect(manifest.name).toBe('Codex Voice');
});

test('service worker route serves javascript', async ({ request }) => {
  const res = await request.get('/web-sw.js');
  expect(res.status()).toBe(200);
  expect(res.headers()['content-type']).toContain('javascript');
  const body = await res.text();
  expect(body.length).toBeGreaterThan(0);
});

test('theme setting persists across a reload', async ({ page }) => {
  await page.locator('#settings-toggle').click();
  const theme = page.locator('#theme');
  await expect(theme).toBeVisible();
  await theme.selectOption('light');

  // The change handler persists the full settings object to localStorage.
  await expect
    .poll(async () => {
      const raw = await page.evaluate((k) => localStorage.getItem(k), SETTINGS_KEY);
      return raw ? JSON.parse(raw).theme : null;
    })
    .toBe('light');

  await page.reload();
  await page.locator('#settings-toggle').click();
  await expect(page.locator('#theme')).toHaveValue('light');
});

test('service worker takes control of /web (offline-capable scope)', async ({ page }) => {
  // Regression guard for the scope mismatch: the app's canonical URL is /web
  // (no trailing slash) while the worker script lives at /web/sw.js, whose
  // default scope /web/ does NOT cover /web. The app registers with an explicit
  // scope of /web, authorized by the Service-Worker-Allowed header. If either
  // side regresses, navigator.serviceWorker.ready never resolves here because
  // no registration's scope matches the document URL.
  await page.goto('/web');
  const scope = await page.evaluate(async () => {
    const registration = await Promise.race([
      navigator.serviceWorker.ready,
      new Promise<never>((_, reject) =>
        setTimeout(() => reject(new Error('no service worker registration matched /web')), 15_000),
      ),
    ]);
    return registration.scope;
  });
  expect(new URL(scope).pathname).toBe('/web');

  // After a reload the (now active) worker must actually control the page.
  await page.reload();
  await expect
    .poll(() => page.evaluate(() => navigator.serviceWorker.controller !== null), {
      timeout: 15_000,
    })
    .toBe(true);
});
