import { expect, test, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';

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

test('mocked generation supports waveform, playback, seeking, and download', async ({
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

});

test('empty paste is a no-op and clear cancels a pending job', async ({ page }) => {
  const harness = await installSpeechHarness(page, false);
  await page.goto('/web?pending-audio=1');
  await page.locator('#text').fill('keep this active draft');
  await page.locator('#generate').click();
  await expect.poll(() => harness.inputs.length).toBe(1);
  await expect(page.locator('#generate')).toBeEnabled();
  await expect(page.locator('#generate')).toHaveAttribute('aria-label', 'Stop generation');
  await expect(page.locator('#generate-label').locator('span')).toHaveText([
    'Generating...',
    'Tap to Stop',
  ]);

  await page.locator('#settings-toggle').click();
  for (const id of ['provider', 'voice', 'model', 'emotion', 'summarize']) {
    await expect(page.locator(`#${id}`)).toBeDisabled();
  }
  await expect(page.locator('#theme')).toBeEnabled();
  await expect(page.locator('#generate-on-paste')).toBeEnabled();

  await page.evaluate(() => navigator.clipboard.writeText(''));
  await page.locator('#paste').click();
  await expect(page.locator('#text')).toHaveValue('keep this active draft');
  await expect(page.locator('#generate')).toBeEnabled();

  await page.locator('#clear').click();
  await expect(page.locator('#text')).toHaveValue('');
  await expect(page.locator('#generate')).toBeEnabled();
  await expect.poll(() => harness.deleted).toContain('job-1');
  await expect(page.locator('#play')).toBeDisabled();
  await expect(page.locator('#download')).toBeDisabled();
});

test('generate button cancels a pending job', async ({ page }) => {
  const harness = await installSpeechHarness(page, false);
  await page.setViewportSize({ width: 320, height: 568 });
  await page.goto('/web?pending-audio=1');
  await page.locator('#text').fill('stop this active draft');
  await page.locator('#generate').click();
  await expect.poll(() => harness.inputs.length).toBe(1);
  await expect(page.locator('#generate')).toHaveAttribute('aria-label', 'Stop generation');
  const buttonBox = await page.locator('#generate').boundingBox();
  const labelBox = await page.locator('#generate-label').boundingBox();
  const spinnerBox = await page.locator('#generate .spinner').boundingBox();
  expect(buttonBox).not.toBeNull();
  expect(labelBox).not.toBeNull();
  expect(spinnerBox).not.toBeNull();
  expect(Math.abs(labelBox!.x + labelBox!.width / 2 - (buttonBox!.x + buttonBox!.width / 2))).toBeLessThan(1);
  expect(labelBox!.x).toBeGreaterThanOrEqual(buttonBox!.x);
  expect(labelBox!.x + labelBox!.width).toBeLessThanOrEqual(buttonBox!.x + buttonBox!.width);
  expect(spinnerBox!.x + spinnerBox!.width).toBeLessThanOrEqual(labelBox!.x);

  await page.locator('#generate').click();

  await expect.poll(() => harness.deleted).toContain('job-1');
  await expect(page.locator('#generate-label')).toHaveText('Generate');
  await expect(page.locator('#text')).toHaveValue('stop this active draft');
});

test('two generate clicks in one event turn cancel without duplicate requests', async ({ page }) => {
  const harness = await installSpeechHarness(page, false);
  await page.goto('/web?rapid-cancel=1');
  await page.locator('#text').fill('cancel before the lazy pipeline starts');

  await page.locator('#generate').evaluate((button) => {
    (button as HTMLButtonElement).click();
    (button as HTMLButtonElement).click();
  });

  await expect(page.locator('#generate-label')).toHaveText('Generate');
  await page.waitForTimeout(50);
  expect(harness.inputs.length).toBeLessThanOrEqual(1);
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
    await expect
      .poll(() => page.locator('main').evaluate((main) => main.scrollTop))
      .toBeGreaterThan(0);

    await page.locator('html').evaluate((root) => root.classList.add('keyboard-open'));
    await page.locator('#emotion').scrollIntoViewIfNeeded();
    await expect(page.locator('#emotion')).toBeVisible();
  });
}

test('declared favicon loads without a console error', async ({ page }) => {
  await page.route('**/web/config', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        version: 2,
        defaultProvider: 'google',
        providers: {},
        personas: {},
      }),
    }),
  );
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
