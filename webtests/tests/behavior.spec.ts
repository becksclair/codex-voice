import { expect, test, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';

const GENERATION_KEY = 'codex-voice.web.generation.v1';

function testWavBase64(durationSeconds = 3, sampleRate = 8_000): string {
  const samples = durationSeconds * sampleRate;
  const dataBytes = samples * 2;
  const wav = Buffer.alloc(44 + dataBytes);
  wav.write('RIFF', 0);
  wav.writeUInt32LE(36 + dataBytes, 4);
  wav.write('WAVE', 8);
  wav.write('fmt ', 12);
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20);
  wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(sampleRate, 24);
  wav.writeUInt32LE(sampleRate * 2, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write('data', 36);
  wav.writeUInt32LE(dataBytes, 40);
  for (let index = 0; index < samples; index += 1) {
    const sample = Math.round(Math.sin((index / sampleRate) * Math.PI * 2 * 440) * 8_000);
    wav.writeInt16LE(sample, 44 + index * 2);
  }
  return wav.toString('base64');
}

interface SpeechHarness {
  inputs: string[];
  deleted: string[];
  complete: boolean;
}

async function installSpeechHarness(page: Page, complete = true): Promise<SpeechHarness> {
  const harness: SpeechHarness = { inputs: [], deleted: [], complete };
  await page.route('**/*', async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (pathname === '/web/config') {
      return route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
    }
    if (pathname === '/web/speech-jobs' && request.method() === 'POST') {
      harness.inputs.push((request.postDataJSON() as { input: string }).input);
      const id = `job-${harness.inputs.length}`;
      return route.fulfill({
        status: 201,
        contentType: 'application/json',
        body: JSON.stringify({ id, status: 'pending' }),
      });
    }
    const jobMatch = pathname.match(/^\/web\/speech-jobs\/(.+)$/);
    if (jobMatch && request.method() === 'DELETE') {
      harness.deleted.push(decodeURIComponent(jobMatch[1]));
      return route.fulfill({ status: 204, body: '' });
    }
    if (jobMatch && request.method() === 'GET') {
      const id = decodeURIComponent(jobMatch[1]);
      const index = Math.max(0, Number(id.split('-').at(-1)) - 1);
      const input = harness.inputs[index] ?? 'resumed draft';
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(
          harness.complete
            ? {
                id,
                status: 'complete',
                result: {
                  input,
                  input_changed: false,
                  audio_base64: testWavBase64(),
                  mime_type: 'audio/wav',
                  format: 'wav',
                },
              }
            : { id, status: 'pending', phase: 'running' },
        ),
      });
    }
    return route.continue();
  });
  return harness;
}

test.use({ serviceWorkers: 'block' });

test('mocked generation supports waveform, playback, seeking, download, and restore', async ({
  page,
}) => {
  const harness = await installSpeechHarness(page);
  await page.goto('/web?deterministic-audio=1');

  await page.locator('#text').fill('deterministic browser audio');
  await page.locator('#generate').click();
  await expect(page.locator('#download')).toBeEnabled();
  await expect(page.locator('#play')).toBeEnabled();
  expect(harness.inputs).toEqual(['deterministic browser audio']);

  await expect
    .poll(() => page.locator('#waveform-slider').getAttribute('aria-valuemax'))
    .not.toBe('0');
  const box = await page.locator('#waveform-slider').boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.click(box!.x + box!.width * 0.65, box!.y + box!.height / 2);
  await expect
    .poll(async () => Number(await page.locator('#waveform-slider').getAttribute('aria-valuenow')))
    .toBeGreaterThan(0);
  await page.locator('#waveform-slider').press('Home');
  await expect(page.locator('#waveform-slider')).toHaveAttribute('aria-valuenow', '0');
  await page.locator('#waveform-slider').press('End');
  await expect
    .poll(async () => Number(await page.locator('#waveform-slider').getAttribute('aria-valuenow')))
    .toBeGreaterThan(2);
  await page.locator('#waveform-slider').press('Home');

  await page.locator('#play').click();
  await expect(page.locator('#play')).toHaveAttribute('aria-label', 'Pause');
  await page.locator('#play').click();
  await expect(page.locator('#play')).toHaveAttribute('aria-label', 'Play');

  const downloadPromise = page.waitForEvent('download');
  await page.locator('#download').click();
  const download = await downloadPromise;
  const bytes = await readFile(await download.path());
  expect(bytes.subarray(0, 4).toString('ascii')).toBe('RIFF');

  await page.reload();
  await expect(page.locator('#text')).toHaveValue('deterministic browser audio');
  await expect(page.locator('#download')).toBeEnabled();
  await expect(page.locator('#play')).toBeEnabled();
});

test('empty paste is a no-op and clear cancels a pending job', async ({ page }) => {
  const harness = await installSpeechHarness(page, false);
  await page.goto('/web?pending-audio=1');
  await page.locator('#text').fill('keep this active draft');
  await page.locator('#generate').click();
  await expect.poll(() => harness.inputs.length).toBe(1);
  await expect(page.locator('#generate')).toBeDisabled();

  await page.locator('#settings-toggle').click();
  for (const id of ['provider', 'voice', 'model', 'emotion', 'summarize']) {
    await expect(page.locator(`#${id}`)).toBeDisabled();
  }
  await expect(page.locator('#theme')).toBeEnabled();
  await expect(page.locator('#generate-on-paste')).toBeEnabled();

  await page.evaluate(() => navigator.clipboard.writeText(''));
  await page.locator('#paste').click();
  await expect(page.locator('#text')).toHaveValue('keep this active draft');
  await expect(page.locator('#generate')).toBeDisabled();

  await page.locator('#clear').click();
  await expect(page.locator('#text')).toHaveValue('');
  await expect(page.locator('#generate')).toBeEnabled();
  await expect.poll(() => harness.deleted).toContain('job-1');
  await expect
    .poll(() => page.evaluate((key) => localStorage.getItem(key), GENERATION_KEY))
    .toBeNull();
  await expect(page.locator('#play')).toBeDisabled();
  await expect(page.locator('#download')).toBeDisabled();
});

test('a pending server job resumes after reload', async ({ page }) => {
  const harness = await installSpeechHarness(page);
  harness.inputs.push('resumed draft');
  await page.addInitScript(
    ({ key, startedAt }) => {
      localStorage.clear();
      localStorage.setItem(
        key,
        JSON.stringify({ input: 'resumed draft', jobId: 'job-1', startedAt }),
      );
    },
    { key: GENERATION_KEY, startedAt: Date.now() },
  );
  await page.goto('/web?resume-audio=1');

  await expect(page.locator('#text')).toHaveValue('resumed draft');
  await expect(page.locator('#download')).toBeEnabled();
  await expect
    .poll(() => page.evaluate((key) => localStorage.getItem(key), GENERATION_KEY))
    .toBeNull();
});

test('generation owners are unique across same-origin pages', async ({ page, context }) => {
  const secondPage = await context.newPage();
  await installSpeechHarness(page, false);
  await installSpeechHarness(secondPage, false);
  await page.goto('/web?owner-page=1');
  await secondPage.goto('/web?owner-page=2');

  await page.locator('#text').fill('first page');
  await page.locator('#generate').click();
  const firstOwner = await expect
    .poll(() =>
      page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null')?.owner, GENERATION_KEY),
    )
    .not.toBeUndefined()
    .then(() =>
      page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null').owner, GENERATION_KEY),
    );

  await secondPage.locator('#text').fill('second page');
  await secondPage.locator('#generate').click();
  await expect
    .poll(() =>
      secondPage.evaluate(
        (key) => JSON.parse(localStorage.getItem(key) ?? 'null')?.input,
        GENERATION_KEY,
      ),
    )
    .toBe('second page');
  const secondOwner = await secondPage.evaluate(
    (key) => JSON.parse(localStorage.getItem(key) ?? 'null').owner,
    GENERATION_KEY,
  );

  expect(secondOwner).not.toBe(firstOwner);
  await page.locator('#clear').click();
  await secondPage.locator('#clear').click();
  await secondPage.close();
});

for (const viewport of [
  { width: 390, height: 667 },
  { width: 320, height: 568 },
]) {
  test(`settings remain reachable at ${viewport.width}x${viewport.height}`, async ({ page }) => {
    await page.setViewportSize(viewport);
    await installSpeechHarness(page);
    await page.goto('/web?responsive-settings=1');
    await page.locator('#settings-toggle').click();
    await page.locator('#generate-on-paste').scrollIntoViewIfNeeded();
    await expect(page.locator('#generate-on-paste')).toBeVisible();
    await expect.poll(() => page.locator('main').evaluate((main) => main.scrollTop)).toBeGreaterThan(0);

    await page.locator('html').evaluate((root) => root.classList.add('keyboard-open'));
    await page.locator('#emotion').scrollIntoViewIfNeeded();
    await expect(page.locator('#emotion')).toBeVisible();
  });
}

test('declared favicon loads without a console error', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });
  await page.goto('/web?favicon-check=1');
  const href = await page.locator('link[rel="icon"]').getAttribute('href');
  expect(href).toBe('/web/icon-192.png');
  const response = await page.request.get(href!);
  expect(response.ok()).toBe(true);
  expect(errors).toEqual([]);
});
