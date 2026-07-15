import { expect, test, type TestInfo } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

import { performanceTagsPreserveText } from '../../web/src/lib/prep/tags.ts';

const SOURCE_URL = 'https://www.gutenberg.org/files/84/84-h/84-h.htm#chap05';
const DEFAULT_BENCHMARK_MODELS = ['gpt-5.6-luna', 'gpt-5.6-terra', 'gpt-5.3-codex-spark'] as const;
const BENCHMARK_MODELS = (
  process.env.ENRICHMENT_MODELS?.split(',') ?? [...DEFAULT_BENCHMARK_MODELS]
).filter(Boolean);
const BENCHMARK_LABEL = (process.env.ENRICHMENT_BENCHMARK_LABEL ?? 'baseline').replace(
  /[^a-z0-9-]+/gi,
  '-',
);
const REASONING_EFFORT = 'none';
const PREP_TIMEOUT_MS = 60_000;
const MIN_TAGS_PER_1000_CHARS = 8;
const MAX_TAGS_PER_1000_CHARS = 13;
const MIN_UNIQUE_TAG_RATIO = 0.75;
const MAX_SINGLE_TAG_SHARE = 0.25;
const MAX_UNTAGGED_GAP_CHARS = 400;
const MIN_TAGS_PER_THIRD = 2;
const MAX_SEMANTIC_ANCHOR_DISTANCE_CHARS = 220;
const MIN_SEMANTIC_ANCHORS_COVERED = 4;

// Mary Wollstonecraft Shelley's Frankenstein, Chapter 5. Project Gutenberg
// eBook #84 is public domain in the USA. This passage is intentionally natural
// literary prose rather than a fixture engineered around the local tagger.
const EMOTIONALLY_CHARGED_PASSAGE = [
  'It was on a dreary night of November that I beheld the accomplishment of my toils. With an anxiety that almost amounted to agony, I collected the instruments of life around me, that I might infuse a spark of being into the lifeless thing that lay at my feet. It was already one in the morning; the rain pattered dismally against the panes, and my candle was nearly burnt out, when, by the glimmer of the half-extinguished light, I saw the dull yellow eye of the creature open; it breathed hard, and a convulsive motion agitated its limbs.',
  'How can I describe my emotions at this catastrophe, or how delineate the wretch whom with such infinite pains and care I had endeavoured to form? His limbs were in proportion, and I had selected his features as beautiful. Beautiful! Great God! His yellow skin scarcely covered the work of muscles and arteries beneath; his hair was of a lustrous black, and flowing; his teeth of a pearly whiteness; but these luxuriances only formed a more horrid contrast with his watery eyes, that seemed almost of the same colour as the dun-white sockets in which they were set, his shrivelled complexion and straight black lips.',
].join('\n\n');

interface TagObservation {
  tag: string;
  offset: number;
  position: number;
  third: 'opening' | 'middle' | 'closing';
  context: string;
}

interface BenchmarkResult {
  model: string;
  elapsedMs: number;
  enriched: string;
  observations: TagObservation[];
  uniqueTags: string[];
  thirds: Record<TagObservation['third'], number>;
  maxGapChars: number;
  awkwardDeterminerInsertions: number;
  missingSpaceAfterTag: number;
  adjacentTagPairs: number;
  wordingPreserved: boolean;
  exactTextPreserved: boolean;
}

interface SemanticAnchor {
  text: string;
  expectedCue: RegExp;
}

const SEMANTIC_ANCHORS: SemanticAnchor[] = [
  {
    text: 'With an anxiety that almost amounted to agony',
    expectedCue: /anxi|agoni|dread|uneas|strain|tens|apprehen|fear/i,
  },
  {
    text: 'I saw the dull yellow eye of the creature open',
    expectedCue: /shock|horr|dread|alarm|fear|shak|gasp|startl|revuls/i,
  },
  {
    text: 'Beautiful!',
    expectedCue: /disbel|horr|shock|revuls|bitter|incred|stun|flat|ironi/i,
  },
  {
    text: 'Great God!',
    expectedCue: /horr|shock|revuls|disbel|gasp|aghast|appall/i,
  },
  {
    text: 'but these luxuriances only formed a more horrid contrast',
    expectedCue: /horr|revuls|disgust|grim|dread|appall|disturb/i,
  },
];

const SEMANTIC_MISMATCHES =
  /sorrow|choked|urgent|playful|delight|amus|laugh|affection|relie|cheer|joy/i;

function observeTags(enriched: string): TagObservation[] {
  return [...enriched.matchAll(/\[[^\]\n]{1,80}\]/g)].map((match) => {
    const offset = match.index ?? 0;
    const position = offset / Math.max(1, enriched.length);
    const third = position < 1 / 3 ? 'opening' : position < 2 / 3 ? 'middle' : 'closing';
    const contextStart = Math.max(0, offset - 55);
    const contextEnd = Math.min(enriched.length, offset + match[0].length + 75);
    return {
      tag: match[0],
      offset,
      position,
      third,
      context: enriched.slice(contextStart, contextEnd).replace(/\s+/g, ' ').trim(),
    };
  });
}

function summarize(model: string, elapsedMs: number, enriched: string): BenchmarkResult {
  const observations = observeTags(enriched);
  const thirds = { opening: 0, middle: 0, closing: 0 };
  for (const observation of observations) thirds[observation.third] += 1;
  const offsets = [0, ...observations.map(({ offset }) => offset), enriched.length];
  const maxGapChars = Math.max(...offsets.slice(1).map((offset, index) => offset - offsets[index]));
  return {
    model,
    elapsedMs,
    enriched,
    observations,
    uniqueTags: [...new Set(observations.map(({ tag }) => tag.toLowerCase()))],
    thirds,
    maxGapChars,
    awkwardDeterminerInsertions: [...enriched.matchAll(/\b(?:a|an|the)\s+\[[^\]\n]{1,80}\]/gi)]
      .length,
    missingSpaceAfterTag: [...enriched.matchAll(/\](?=[^\s\[])/g)].length,
    adjacentTagPairs: [...enriched.matchAll(/\]\s*\[/g)].length,
    wordingPreserved: performanceTagsPreserveText(EMOTIONALLY_CHARGED_PASSAGE, enriched),
    exactTextPreserved:
      enriched
        .replace(/\[[^\]\n]{1,80}\]/g, '')
        .replace(/\s+/g, ' ')
        .trim() === EMOTIONALLY_CHARGED_PASSAGE.replace(/\s+/g, ' ').trim(),
  };
}

function semanticAnchorCoverage(result: BenchmarkResult): Array<{
  anchor: string;
  tag?: string;
  distance?: number;
  covered: boolean;
}> {
  return SEMANTIC_ANCHORS.map(({ text, expectedCue }) => {
    const anchorOffset = result.enriched.indexOf(text);
    const preceding = result.observations.filter(({ offset }) => offset < anchorOffset).at(-1);
    const distance = preceding
      ? anchorOffset - (preceding.offset + preceding.tag.length)
      : undefined;
    return {
      anchor: text,
      tag: preceding?.tag,
      distance,
      covered:
        anchorOffset >= 0 &&
        preceding !== undefined &&
        distance !== undefined &&
        distance <= MAX_SEMANTIC_ANCHOR_DISTANCE_CHARS &&
        expectedCue.test(preceding.tag),
    };
  });
}

function placementTable(result: BenchmarkResult): string {
  return result.observations
    .map(
      ({ tag, position, third, context }, index) =>
        `| ${index + 1} | ${tag} | ${(position * 100).toFixed(1)}% | ${third} | ${context.replaceAll('|', '\\|')} |`,
    )
    .join('\n');
}

function writeBenchmarkArtifact(testInfo: TestInfo, results: BenchmarkResult[]): string {
  const summaryRows = results
    .map(
      (result) =>
        `| ${result.model} | ${result.elapsedMs} | ${result.observations.length} | ${result.uniqueTags.length} | ${result.thirds.opening}/${result.thirds.middle}/${result.thirds.closing} | ${result.maxGapChars} | ${result.missingSpaceAfterTag} | ${result.adjacentTagPairs} | ${result.wordingPreserved} |`,
    )
    .join('\n');
  const details = results
    .map(
      (result) => `## ${result.model}

- Reasoning effort: ${REASONING_EFFORT}
- Tags: ${result.observations.length}
- Unique tags: ${result.uniqueTags.join(', ')}
- Distribution: opening ${result.thirds.opening}, middle ${result.thirds.middle}, closing ${result.thirds.closing}
- Maximum gap: ${result.maxGapChars} characters
- Awkward determiner insertions: ${result.awkwardDeterminerInsertions}
- Missing spaces after tags: ${result.missingSpaceAfterTag}
- Adjacent tag pairs: ${result.adjacentTagPairs}
- Wording preserved: ${result.wordingPreserved}
- Exact source text preserved: ${result.exactTextPreserved}

### Tag placement

| # | Tag | Position | Third | Context |
|---:|---|---:|---|---|
${placementTable(result)}

### Enriched text

${result.enriched}`,
    )
    .join('\n\n');
  const report = `# E2E Emotion Enrichment Model Benchmark

- Source: Mary Wollstonecraft Shelley, *Frankenstein*, Chapter 5
- Public-domain text: ${SOURCE_URL}
- Source characters: ${EMOTIONALLY_CHARGED_PASSAGE.length}
- Benchmark label: ${BENCHMARK_LABEL}
- Reasoning effort: ${REASONING_EFFORT}
- Audio synthesis: disabled

## Descriptive comparison

| Model | Time ms | Tags | Unique | Opening/Middle/Closing | Max gap chars | Missing spaces | Adjacent pairs | Preserved |
|---|---:|---:|---:|---|---:|---:|---:|---|
${summaryRows}

${details}

## Original

${EMOTIONALLY_CHARGED_PASSAGE}
`;
  const artifactPath = resolve(
    testInfo.config.rootDir,
    `../benchmark-results/enrichment-benchmark-${BENCHMARK_LABEL}.md`,
  );
  mkdirSync(dirname(artifactPath), { recursive: true });
  writeFileSync(artifactPath, report);
  return artifactPath;
}

test.describe('live emotion enrichment benchmark', () => {
  test.skip(
    process.env.LIVE_ENRICHMENT_BENCHMARK !== '1',
    'set LIVE_ENRICHMENT_BENCHMARK=1 to run the paid prep-only benchmark',
  );

  test('compares selected models without richness thresholds', async ({ request }, testInfo) => {
    test.setTimeout(300_000);
    const configResponse = await request.get('/web/config', {
      headers: { 'cache-control': 'no-store' },
    });
    test.skip(configResponse.status() === 503, 'live TTS config is unavailable');
    expect(configResponse.ok()).toBeTruthy();

    const results: BenchmarkResult[] = [];
    for (const model of BENCHMARK_MODELS) {
      const startedAt = performance.now();
      const response = await request.post('/web/speech-prep', {
        data: {
          input: EMOTIONALLY_CHARGED_PASSAGE,
          provider: 'google',
          speechPrepEnabled: true,
          speechPrepModel: model,
          speechPrepReasoningEffort: REASONING_EFFORT,
          speechPrepTimeoutMs: PREP_TIMEOUT_MS,
        },
        timeout: 90_000,
      });
      const responseText = await response.text();
      expect(response.ok(), `${model}: ${responseText}`).toBeTruthy();
      const payload = JSON.parse(responseText) as { input: string; input_changed: boolean };
      const result = summarize(model, Math.round(performance.now() - startedAt), payload.input);

      // Phase one locks only strict model completion and preservation invariants.
      expect(payload.input_changed, `${model} returned unchanged text`).toBe(true);
      expect(result.observations.length, `${model} returned no valid tags`).toBeGreaterThan(0);
      expect(result.wordingPreserved, `${model} changed the source wording`).toBe(true);
      results.push(result);
      writeBenchmarkArtifact(testInfo, results);
    }

    const artifactPath = writeBenchmarkArtifact(testInfo, results);
    console.log(`ENRICHMENT_BENCHMARK_ARTIFACT=${artifactPath}`);
    console.log(
      `ENRICHMENT_BENCHMARK_SUMMARY=${JSON.stringify(
        results.map((result) => ({
          model: result.model,
          elapsedMs: result.elapsedMs,
          tags: result.observations.length,
          unique: result.uniqueTags.length,
          thirds: result.thirds,
          maxGapChars: result.maxGapChars,
          awkwardDeterminerInsertions: result.awkwardDeterminerInsertions,
          missingSpaceAfterTag: result.missingSpaceAfterTag,
          adjacentTagPairs: result.adjacentTagPairs,
        })),
      )}`,
    );
  });
});

test.describe('configured emotion enrichment quality', () => {
  test.skip(
    process.env.LIVE_ENRICHMENT_QUALITY !== '1',
    'set LIVE_ENRICHMENT_QUALITY=1 to run the paid prep-only quality gate',
  );

  test('meets the configured Luna richness and semantic-quality contract', async ({
    request,
  }, testInfo) => {
    test.setTimeout(120_000);
    const configResponse = await request.get('/web/config', {
      headers: { 'cache-control': 'no-store' },
    });
    test.skip(configResponse.status() === 503, 'live TTS config is unavailable');
    expect(configResponse.ok()).toBeTruthy();
    const config = (await configResponse.json()) as {
      speechPrep?: { model?: string; reasoningEffort?: string };
    };
    expect(config.speechPrep?.model).toBe('gpt-5.6-luna');
    // The Rust config normalizes explicit `none` to an omitted API field;
    // non-none efforts remain observable as strings.
    expect(config.speechPrep?.reasoningEffort).toBeUndefined();

    const startedAt = performance.now();
    const response = await request.post('/web/speech-prep', {
      data: {
        input: EMOTIONALLY_CHARGED_PASSAGE,
        provider: 'google',
        speechPrepEnabled: true,
        speechPrepTimeoutMs: PREP_TIMEOUT_MS,
      },
      timeout: 90_000,
    });
    const responseText = await response.text();
    expect(response.ok(), responseText).toBeTruthy();
    const payload = JSON.parse(responseText) as { input: string; input_changed: boolean };
    const result = summarize(
      config.speechPrep.model,
      Math.round(performance.now() - startedAt),
      payload.input,
    );
    const density = (result.observations.length * 1000) / EMOTIONALLY_CHARGED_PASSAGE.length;
    const uniqueRatio = result.uniqueTags.length / Math.max(1, result.observations.length);
    const tagCounts = result.observations.reduce<Record<string, number>>((counts, { tag }) => {
      const normalized = tag.toLowerCase();
      counts[normalized] = (counts[normalized] ?? 0) + 1;
      return counts;
    }, {});
    const maxSingleTagShare =
      Math.max(0, ...Object.values(tagCounts)) / Math.max(1, result.observations.length);
    const anchorCoverage = semanticAnchorCoverage(result);
    const coveredAnchors = anchorCoverage.filter(({ covered }) => covered).length;
    const semanticMismatches = result.observations.filter(({ tag }) =>
      SEMANTIC_MISMATCHES.test(tag),
    );

    writeBenchmarkArtifact(testInfo, [result]);
    console.log(
      `ENRICHMENT_QUALITY_METRICS=${JSON.stringify({
        model: result.model,
        elapsedMs: result.elapsedMs,
        tags: result.observations.length,
        densityPer1000Chars: Number(density.toFixed(2)),
        uniqueTags: result.uniqueTags.length,
        uniqueRatio: Number(uniqueRatio.toFixed(2)),
        maxSingleTagShare: Number(maxSingleTagShare.toFixed(2)),
        thirds: result.thirds,
        maxGapChars: result.maxGapChars,
        awkwardDeterminerInsertions: result.awkwardDeterminerInsertions,
        missingSpaceAfterTag: result.missingSpaceAfterTag,
        adjacentTagPairs: result.adjacentTagPairs,
        wordingPreserved: result.wordingPreserved,
        exactTextPreserved: result.exactTextPreserved,
        semanticAnchorsCovered: coveredAnchors,
        semanticAnchorsTotal: anchorCoverage.length,
        anchorCoverage,
        semanticMismatches: semanticMismatches.map(({ tag }) => tag),
      })}`,
    );

    expect.soft(payload.input_changed).toBe(true);
    expect.soft(result.wordingPreserved).toBe(true);
    expect.soft(result.exactTextPreserved).toBe(true);
    expect.soft(density).toBeGreaterThanOrEqual(MIN_TAGS_PER_1000_CHARS);
    expect.soft(density).toBeLessThanOrEqual(MAX_TAGS_PER_1000_CHARS);
    expect.soft(uniqueRatio).toBeGreaterThanOrEqual(MIN_UNIQUE_TAG_RATIO);
    expect.soft(maxSingleTagShare).toBeLessThanOrEqual(MAX_SINGLE_TAG_SHARE);
    expect.soft(result.maxGapChars).toBeLessThanOrEqual(MAX_UNTAGGED_GAP_CHARS);
    expect.soft(result.thirds.opening).toBeGreaterThanOrEqual(MIN_TAGS_PER_THIRD);
    expect.soft(result.thirds.middle).toBeGreaterThanOrEqual(MIN_TAGS_PER_THIRD);
    expect.soft(result.thirds.closing).toBeGreaterThanOrEqual(MIN_TAGS_PER_THIRD);
    expect.soft(result.awkwardDeterminerInsertions).toBe(0);
    expect.soft(result.missingSpaceAfterTag).toBe(0);
    expect.soft(result.adjacentTagPairs).toBe(0);
    expect.soft(semanticMismatches).toEqual([]);
    expect.soft(coveredAnchors).toBeGreaterThanOrEqual(MIN_SEMANTIC_ANCHORS_COVERED);
  });
});
