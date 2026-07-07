import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';

// Import the REAL chunking algorithm from the web source so the chunk-count
// expectation below is validated against production code, not a copy.
import { splitTtsText } from '../../web/src/lib/synth/chunking.ts';

// Storage key used by the PWA (see web/src/lib/storage.ts / settings.ts).
const SETTINGS_KEY = 'codex-voice.web.settings.v1';

/**
 * Crafted natural-language input for the paid Google leg.
 *
 * At 1922 codepoints it clears TTS_CHUNK_MIN_CHARS (1600), so the browser-direct
 * Google path exercises the chunking + stitching code. With TTS_CHUNK_MAX_CHARS
 * (900) it splits into exactly THREE chunks (lengths ~792 / ~812 / ~316) — see
 * the assertion in the first stage, which recomputes this with the real
 * splitTtsText. This is the deliberate "assertions-per-dollar" compromise: large
 * enough to hit chunking, small enough to keep the paid run cheap (~1.9k chars).
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

/** Expected chunk count for LIVE_SMOKE_INPUT, recomputed live from splitTtsText. */
const EXPECTED_CHUNKS = 3;

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
  test.skip(!process.env.LIVE_TTS, 'set LIVE_TTS=1 to run the paid live TTS smoke');

  test('google direct (chunked) + server job in one session', async ({ page }) => {
    // --- Stage 0: chunking expectation, validated against the real algorithm ---
    // Free assertion (no API cost): confirms the crafted input still splits the
    // way this smoke assumes before we spend anything.
    expect(splitTtsText(LIVE_SMOKE_INPUT).length).toBe(EXPECTED_CHUNKS);

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
    // Generous timeout: three sequential Google requests + stitching.
    await expect(page.locator('#download')).toBeEnabled({ timeout: 120_000 });
    await expect(page.locator('#play')).toBeEnabled();

    // --- Stage 6: no error surfaced ---
    await expect(page.locator('#error-banner')).toBeHidden();
    expect((await page.locator('#error-banner').textContent())?.trim() || '').toBe('');

    // --- Stage 7: plausible duration for ~1.9k chars of speech (> 10s) ---
    await expect
      .poll(() => page.locator('#duration').textContent().then((t) => parseTimecode(t) ?? 0), {
        timeout: 30_000,
      })
      .toBeGreaterThan(10);

    // --- Stage 8: waveform canvas actually drew something (non-uniform pixels) ---
    const waveformDrawn = await page.locator('#waveform').evaluate((el) => {
      const canvas = el as HTMLCanvasElement;
      const ctx = canvas.getContext('2d');
      if (!ctx || !canvas.width || !canvas.height) return false;
      const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);
      for (let i = 4; i < data.length; i += 4) {
        if (
          data[i] !== data[0] ||
          data[i + 1] !== data[1] ||
          data[i + 2] !== data[2] ||
          data[i + 3] !== data[3]
        ) {
          return true;
        }
      }
      return false;
    });
    expect(waveformDrawn).toBe(true);

    // --- Stage 9: playback starts (elapsed advances past 0:00) ---
    await page.locator('#play').click();
    await expect
      .poll(() => page.locator('#elapsed').textContent().then((t) => parseTimecode(t) ?? 0), {
        timeout: 20_000,
      })
      .toBeGreaterThan(0);

    // --- Stage 10: download yields a valid WAV (RIFF magic + > 100KB) ---
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

    // --- Stage 11: server-job path (POST /web/speech-jobs → poll → decode) ---
    const create = await page.request.post('/web/speech-jobs', {
      data: { input: 'Short live smoke line.' },
    });
    expect(create.ok()).toBeTruthy();
    const created = (await create.json()) as { id?: string; status?: string };
    expect(created.id, 'server job should return an id').toBeTruthy();

    let jobResult: { audio_base64?: string; mime_type?: string; format?: string } | undefined;
    await expect
      .poll(
        async () => {
          const status = await page.request.get(`/web/speech-jobs/${created.id}`, {
            headers: { 'cache-control': 'no-store' },
          });
          expect(status.ok()).toBeTruthy();
          const body = (await status.json()) as {
            status: string;
            result?: typeof jobResult;
            error?: unknown;
          };
          if (body.status === 'failed') {
            throw new Error(`server speech job failed: ${JSON.stringify(body.error)}`);
          }
          if (body.status === 'complete') jobResult = body.result;
          return body.status;
        },
        { timeout: 120_000, intervals: [1_000] },
      )
      .toBe('complete');

    expect(jobResult, 'completed job should carry a result').toBeTruthy();
    expect((jobResult?.audio_base64 ?? '').length).toBeGreaterThan(2_000);
    expect(jobResult?.mime_type ?? '').toMatch(/^audio\//);
    expect((jobResult?.format ?? '').length).toBeGreaterThan(0);
  });

  test('elevenlabs leg (opt-in)', async ({ page }) => {
    // A second paid leg, off by default even when LIVE_TTS=1. ElevenLabs bills
    // separately, so it is gated behind its own flag.
    test.skip(
      !process.env.LIVE_TTS_ELEVENLABS,
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
