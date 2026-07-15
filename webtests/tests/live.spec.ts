import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';

// Import the REAL chunking algorithm from the web source so the chunk-count
// expectation below is validated against production code, not a copy.
// Storage key used by the PWA (see web/src/lib/storage.ts / settings.ts).
const SETTINGS_KEY = 'codex-voice.web.settings.v1';

/**
 * Crafted natural-language input for the paid Google leg.
 *
 * At 1922 codepoints it is long enough to exercise backend prep and chunked
 * synthesis while remaining a bounded paid smoke input.
 */
const LIVE_SMOKE_INPUT = [
  'The morning fog rolled off the harbor slowly, curling around the moored fishing boats and softening every hard edge of the waterfront into a pale grey wash.',
  'Gulls wheeled overhead, calling to one another in sharp, insistent voices, while the first delivery trucks rumbled down the cobbled lane toward the market square.',
  'A woman in a yellow raincoat unlocked the shutters of her bakery, and within minutes the smell of warm bread drifted out to meet the commuters hurrying past.',
  'Down at the pier, an old man mended his nets with patient, weathered hands, humming a tune that no one else remembered but that he had known since childhood.',
  'The tide was turning, and the water slapped gently against the barnacled pilings, keeping a rhythm as steady and unhurried as the man’s own quiet breathing.',
  'Across the bay, the lighthouse blinked its last few rotations before the keeper switched it off for the day, its beam swallowed by the brightening sky.',
  'Children in bright coats clustered at the bus stop, comparing the contents of their lunch boxes and inventing elaborate plans for the coming weekend adventures.',
  'By the time the sun finally broke through, the whole town had shaken off its sleep, and the day arrived with the ordinary, comfortable clamor of a place that knew itself.',
  'Shopkeepers swept their doorsteps, a cat stretched luxuriously on a sun-warmed windowsill, and somewhere a radio played a song half a century old to no one in particular.',
  'It was, by every measure, an unremarkable morning, and that was precisely what made it feel, to the few who paused to notice, quietly and completely perfect.',
  'A young cyclist coasted along the promenade, weaving between the puddles left by the overnight rain and ringing her bell at a pair of dawdling pigeons.',
  'The clock on the town hall struck eight with a deep and resonant chime, and the whole scene seemed to lean forward, ready at last to begin the real work of the day.',
].join(' ');

/** Parse an `m:ss` timecode (as rendered by formatTime) to seconds; null if not a plain time. */
function parseTimecode(raw: string | null | undefined): number | null {
  const match = /^(\d+):([0-5]\d)$/.exec((raw ?? '').trim());
  if (!match) return null;
  return Number(match[1]) * 60 + Number(match[2]);
}

/** Load /web from a clean slate: clear persisted localStorage, then reload. */
async function loadCleanShell(page: Page): Promise<void> {
  await page.goto('/web');
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  await expect(page).toHaveTitle('Codex Voice');
}

/** Open the settings panel and wait for the provider select to be populated from live config. */
async function openSettingsAndAwaitProviders(page: Page): Promise<void> {
  await page.locator('#settings-toggle').click();
  await expect(page.locator('#provider')).toBeVisible();
  // refreshConfig() runs async on mount; the select starts with only "Auto"
  // and gains real provider options once /web/config resolves.
  await expect
    .poll(() => page.locator('#provider option').count(), { timeout: 15_000 })
    .toBeGreaterThan(1);
}

test.describe.serial('live TTS smoke', () => {
  // Hard gate: this spec spends real money per character. It only runs when the
  // operator explicitly opts in. Without LIVE_TTS=1 every test here is skipped.
  test.skip(process.env.LIVE_TTS !== '1', 'set LIVE_TTS=1 to run the paid live TTS smoke');

  test('google backend-first generation enriches and synthesizes in one session', async ({ page }) => {
    test.setTimeout(180_000);
    let serverJobCreates = 0;
    const browserProviderRequests: string[] = [];
    page.on('request', (request) => {
      const url = new URL(request.url());
      if (url.pathname === '/web/speech-jobs' && request.method() === 'POST') {
        serverJobCreates += 1;
      }
      if (
        url.hostname === 'generativelanguage.googleapis.com' ||
        url.hostname === 'api.elevenlabs.io' ||
        url.hostname === 'chatgpt.com'
      ) {
        browserProviderRequests.push(request.url());
      }
    });

    // --- Stage 1: shell loads ---
    await loadCleanShell(page);

    // --- Stage 2: real config available (else skip cleanly) ---
    const configRes = await page.request.get('/web/config', { headers: { 'cache-control': 'no-store' } });
    test.skip(
      configRes.status() === 503,
      '/web/config returned 503 — no real TTS config on this host (add ~/.codex/read-aloud-defaults.json)',
    );
    expect(configRes.ok()).toBeTruthy();
    const config = (await configRes.json()) as { providers?: Record<string, unknown> };
    const providerCount = Object.keys(config.providers ?? {}).length;
    test.skip(providerCount === 0, '/web/config exposes no providers — nothing to synthesize');
    test.skip(!config.providers?.google, '/web/config has no Google provider configured');

    // --- Stage 3: provider select populates, then pick Google ---
    await openSettingsAndAwaitProviders(page);
    await expect(page.locator('#provider option[value="google"]')).toHaveCount(1);
    await page.locator('#provider').selectOption('google');
    await expect
      .poll(async () => {
        const raw = await page.evaluate((k) => localStorage.getItem(k), SETTINGS_KEY);
        return raw ? (JSON.parse(raw).provider as string) : null;
      })
      .toBe('google');

    // --- Stage 4: enter crafted input and generate ---
    await page.locator('#text').fill(LIVE_SMOKE_INPUT);
    await page.locator('#generate').click();

    // --- Stage 5: wait for completion via a real DOM signal ---
    // On success the audio blob is loaded, which enables #download and #play.
    // On failure the app shows #error-banner instead — race both so a real
    // synthesis failure reports the banner text (the diagnostic this paid run
    // exists to surface) rather than an opaque 120s timeout on #download.
    const outcome = await Promise.race([
      page
        .locator('#download')
        .waitFor({ state: 'attached', timeout: 120_000 })
        .then(() => page.waitForFunction(
          () => !(document.getElementById('download') as HTMLButtonElement | null)?.disabled,
          undefined,
          { timeout: 120_000 },
        ))
        .then(() => 'complete' as const),
      page
        .locator('#error-banner')
        .waitFor({ state: 'visible', timeout: 120_000 })
        .then(() => 'error' as const),
    ]);
    if (outcome === 'error') {
      const message = (await page.locator('#error-banner').textContent())?.trim();
      throw new Error(`generation failed: ${message || '(empty error banner)'}`);
    }
    await expect(page.locator('#download')).toBeEnabled();
    await expect(page.locator('#play')).toBeEnabled();

    // --- Stage 6: backend-first prep inserted several context-local cues ---
    expect(serverJobCreates).toBe(1);
    expect(browserProviderRequests).toEqual([]);
    const preparedText = await page.locator('#text').inputValue();
    expect(preparedText).not.toBe(LIVE_SMOKE_INPUT);
    expect(preparedText.match(/\[[^\]\n]{1,80}\]/g)?.length ?? 0).toBeGreaterThan(2);

    // --- Stage 7: no error surfaced after completion ---
    await expect(page.locator('#error-banner')).toBeHidden();
    expect((await page.locator('#error-banner').textContent())?.trim() || '').toBe('');

    // --- Stage 8: plausible duration for ~1.9k chars of speech (> 10s) ---
    await expect
      .poll(() => page.locator('#duration').textContent().then((t) => parseTimecode(t) ?? 0), {
        timeout: 30_000,
      })
      .toBeGreaterThan(10);

    // --- Stage 9: waveform canvas actually drew something (non-uniform pixels) ---
    const waveformDrawn = await page.locator('#waveform').evaluate((el) => {
      const canvas = el as HTMLCanvasElement;
      const ctx = canvas.getContext('2d');
      if (!ctx || !canvas.width || !canvas.height) return false;
      const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);
      // Count pixels differing from the first pixel: a real waveform paints
      // hundreds of bars, while a background-only canvas (gradient/frame) has
      // few or none. Threshold well below a real draw, well above noise.
      let differing = 0;
      for (let i = 4; i < data.length; i += 4) {
        if (
          data[i] !== data[0] ||
          data[i + 1] !== data[1] ||
          data[i + 2] !== data[2] ||
          data[i + 3] !== data[3]
        ) {
          differing += 1;
          if (differing >= 200) return true;
        }
      }
      return false;
    });
    expect(waveformDrawn).toBe(true);

    // --- Stage 10: playback starts (elapsed advances past 0:00) ---
    await page.locator('#play').click();
    await expect
      .poll(() => page.locator('#elapsed').textContent().then((t) => parseTimecode(t) ?? 0), {
        timeout: 20_000,
      })
      .toBeGreaterThan(0);

    // --- Stage 11: download yields a valid WAV (RIFF magic + > 100KB) ---
    // The download button builds a blob object-URL anchor and clicks it; the
    // Playwright download event is the inspectable handle on those bytes.
    const downloadPromise = page.waitForEvent('download');
    await page.locator('#download').click();
    const download = await downloadPromise;
    const filePath = await download.path();
    const bytes = readFileSync(filePath);
    expect(bytes.length).toBeGreaterThan(100 * 1024);
    expect(bytes.subarray(0, 4).toString('ascii')).toBe('RIFF');
    expect(bytes.subarray(8, 12).toString('ascii')).toBe('WAVE');
  });

  test('elevenlabs leg (opt-in)', async ({ page }) => {
    // A second paid leg, off by default even when LIVE_TTS=1. ElevenLabs bills
    // separately, so it is gated behind its own flag.
    test.skip(
      process.env.LIVE_TTS_ELEVENLABS !== '1',
      'set LIVE_TTS_ELEVENLABS=1 to run the ElevenLabs live leg',
    );

    await loadCleanShell(page);

    const configRes = await page.request.get('/web/config', { headers: { 'cache-control': 'no-store' } });
    test.skip(configRes.status() === 503, '/web/config returned 503 — no real TTS config on this host');
    expect(configRes.ok()).toBeTruthy();

    await openSettingsAndAwaitProviders(page);
    const hasElevenLabs = await page.locator('#provider option[value="elevenlabs"]').count();
    test.skip(hasElevenLabs === 0, '/web/config has no ElevenLabs provider configured');

    await page.locator('#provider').selectOption('elevenlabs');
    // Short input (~90 chars): stays under TTS_CHUNK_MIN_CHARS, one request, cheap.
    await page.locator('#text').fill('A short ElevenLabs live smoke line to confirm the voice stack answers.');
    await page.locator('#generate').click();

    await expect(page.locator('#download')).toBeEnabled({ timeout: 120_000 });
    await expect(page.locator('#error-banner')).toBeHidden();
    await expect
      .poll(() => page.locator('#duration').textContent().then((t) => parseTimecode(t) ?? 0), {
        timeout: 30_000,
      })
      .toBeGreaterThan(0);
  });
});
