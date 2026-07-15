/**
 * Performance-tag and style-instruction text analysis for speech prep.
 *
 * Ports the tag-repair/validation helpers from app.html (lines ~2093-2360):
 * word tokenization, preservation checks, bare-cue repair, tag validity, the
 * local fallback tagger, and style-instruction validity. All functions are
 * pure; `prep` is read only for `tagPalette`/`maxLength`/`mode`.
 */

import type { EffectiveSpeechPrep } from "./types.ts";

/** Minimal prep view used by the tag helpers. */
type PrepLike = Pick<EffectiveSpeechPrep, "tagPalette" | "maxLength" | "mode"> | null | undefined;

/** Tokenize into lowercase word tokens, stripping bracket tags. Ports `textWords`. */
export function textWords(value: string | null | undefined): string[] {
  return (
    String(value || "")
      .replace(/\[[^\]]{1,80}\]/g, " ")
      .toLowerCase()
      .match(/[\p{L}\p{M}\p{N}']+/gu) || []
  );
}

/** A word token with its source-string span. */
export interface WordSpan {
  word: string;
  start: number;
  end: number;
}

/**
 * Tokenize into word spans, skipping bracketed tags. Ports `textWordSpans`
 * (app.html line ~2104).
 */
export function textWordSpans(value: string | null | undefined): WordSpan[] {
  const text = String(value || "");
  const spans: WordSpan[] = [];
  let inTag = false;
  let current = "";
  let start = 0;
  let index = 0;
  for (const ch of text) {
    if (ch === "[" && !current) {
      inTag = true;
      index += ch.length;
      continue;
    }
    if (ch === "]" && inTag) {
      inTag = false;
      index += ch.length;
      continue;
    }
    if (inTag) {
      index += ch.length;
      continue;
    }
    if (/[\p{L}\p{M}\p{N}']/u.test(ch)) {
      if (!current) start = index;
      current += ch.toLowerCase();
      index += ch.length;
      continue;
    }
    if (current) {
      spans.push({ word: current, start, end: index });
      current = "";
    }
    index += ch.length;
  }
  if (current) spans.push({ word: current, start, end: text.length });
  return spans;
}

/**
 * Whether the tagged text preserves the original words. Ports
 * `performanceTagsPreserveText` (app.html line ~2135): requires ≥97% of the
 * original words to appear in order and the final word to survive.
 */
export function performanceTagsPreserveText(
  input: string | null | undefined,
  prepared: string | null | undefined,
): boolean {
  const original = textWords(input);
  if (!original.length) return true;
  const tagged = textWords(prepared);
  let found = 0;
  let taggedIndex = 0;
  for (const word of original) {
    while (taggedIndex < tagged.length && tagged[taggedIndex] !== word) taggedIndex += 1;
    if (taggedIndex >= tagged.length) continue;
    found += 1;
    taggedIndex += 1;
  }
  const ratio = found / original.length;
  const tailPreserved = original.length < 3 || tagged.includes(original[original.length - 1]);
  return ratio >= 0.97 && tailPreserved;
}

/** Extract bracketed tags. Ports `bracketTags` (app.html line ~2151). */
export function bracketTags(value: string | null | undefined): string[] {
  return String(value || "").match(/\[[^\]\n]{1,80}\]/g) || [];
}

function stripPrefixIgnoreCase(value: string, prefix: string): string | null {
  return value.toLowerCase().startsWith(prefix.toLowerCase()) ? value.slice(prefix.length) : null;
}

function isBareCueDelimiter(ch: string): boolean {
  return /[:;,.\-!?\s]/.test(ch || "");
}

/** Trim leading/trailing cue delimiters. Ports `cleanBareCue`. */
export function cleanBareCue(value: string | null | undefined): string {
  let cue = String(value || "");
  while (cue && isBareCueDelimiter(cue[0])) cue = cue.slice(1);
  while (cue && isBareCueDelimiter(cue[cue.length - 1])) cue = cue.slice(0, -1);
  return cue.trim();
}

const CUE_WORDS = new Set([
  "affectionate",
  "amused",
  "angry",
  "breathless",
  "calm",
  "chuckle",
  "chuckles",
  "deadpan",
  "dryly",
  "exhale",
  "exhales",
  "fearful",
  "flatly",
  "frustrated",
  "gasp",
  "gasps",
  "hesitates",
  "laugh",
  "laughing",
  "laughs",
  "leans",
  "lowers",
  "kiss",
  "kisses",
  "kissing",
  "lips",
  "moan",
  "moans",
  "nervous",
  "pause",
  "proud",
  "relieved",
  "reassuring",
  "scoffs",
  "serious",
  "shaky",
  "sigh",
  "sighs",
  "sleepy",
  "smile",
  "smiles",
  "smiling",
  "softly",
  "sorrowful",
  "swallows",
  "tender",
  "teasing",
  "urgent",
  "vulnerable",
  "warmly",
  "whisper",
  "whispers",
  "wistful",
]);

/**
 * Whether a phrase reads as a bare (unbracketed) performance cue. Ports
 * `looksLikeBarePerformanceCue` (app.html line ~2172).
 */
export function looksLikeBarePerformanceCue(cue: string, prep: PrepLike): boolean {
  const lower = cleanBareCue(cue).toLowerCase();
  const words = textWords(lower);
  if (!words.length || words.length > 5) return false;
  const palette = new Set((prep?.tagPalette || []).map((tag) => String(tag).toLowerCase()));
  if (palette.has(lower)) return true;
  return words.some((word) => CUE_WORDS.has(word));
}

/** Build the ranked bare-cue phrase list. Ports `barePerformanceCuePhrases`. */
export function barePerformanceCuePhrases(prep: PrepLike): string[] {
  const phrases = [
    "smiles softly",
    "smiles and lowers my voice",
    "smiles and lowers her voice",
    "smiles and lowers his voice",
    "smiles and lowers their voice",
    "lowers my voice",
    "lowers her voice",
    "lowers his voice",
    "lowers their voice",
    "leans over and kisses your lips softly",
    "leans over and kisses her lips softly",
    "leans over and kisses his lips softly",
    "leans over and kisses their lips softly",
    "leans over and kisses you softly",
    "leans over and kisses her softly",
    "leans over and kisses him softly",
    "leans over and kisses them softly",
    "laughs softly",
    "chuckles softly",
    "sighs softly",
    "whispers softly",
    "smiles",
    "smiling",
    "laughs",
    "laughing",
    "chuckles",
    "sighs",
    "sigh",
    "whispers",
    "gasps",
    "exhales",
    "moans",
    "hesitates",
    "swallows",
    "voice breaks",
    "leans closer",
    "under breath",
    "softly",
    "warmly",
    "dryly",
    "flatly",
  ];
  for (const tag of prep?.tagPalette || []) {
    if (looksLikeBarePerformanceCue(tag, prep)) phrases.push(String(tag).toLowerCase());
  }
  return [...new Set(phrases)].sort((a, b) => b.length - a.length);
}

function preservedTextStart(
  input: string | null | undefined,
  prepared: string | null | undefined,
): number | null {
  const original = textWords(input).slice(0, 3);
  if (!original.length) return null;
  const preparedWords = textWordSpans(prepared);
  for (let index = 0; index < preparedWords.length; index += 1) {
    let matched = true;
    for (let offset = 0; offset < original.length; offset += 1) {
      if (preparedWords[index + offset]?.word !== original[offset]) {
        matched = false;
        break;
      }
    }
    if (matched) return preparedWords[index].start;
  }
  return null;
}

function repairLeadingBareCue(input: string, prepared: string, prep: PrepLike): string {
  const value = String(prepared || "");
  const leading = value.match(/^\s*/)?.[0] || "";
  const trimmed = value.slice(leading.length);
  const sourceStart = preservedTextStart(input, trimmed);
  if (!sourceStart) return prepared;
  const cue = cleanBareCue(trimmed.slice(0, sourceStart));
  if (!cue || !looksLikeBarePerformanceCue(cue, prep)) return prepared;
  const body = trimmed.slice(sourceStart).trimStart();
  if (!body) return prepared;
  const repaired = `${leading}[${cue}] ${body}`;
  return performanceTagsPreserveText(input, repaired) ? repaired : prepared;
}

function isSentenceBoundary(value: string, index: number): boolean {
  if (index === 0) return true;
  const prefix = value.slice(0, index);
  let sawNewline = false;
  let cursor = prefix.length - 1;
  while (cursor >= 0 && /\s/.test(prefix[cursor])) {
    sawNewline = sawNewline || prefix[cursor] === "\n";
    cursor -= 1;
  }
  if (sawNewline) return true;
  return /[.!?]/.test(prefix[cursor] || "");
}

function isInsideBracketTag(value: string, index: number): boolean {
  const prefix = value.slice(0, index);
  const open = prefix.lastIndexOf("[");
  return open >= 0 && prefix.slice(open).indexOf("]") < 0;
}

function cueTrailingDelimiterLength(value: string): number | null {
  let length = 0;
  let sawSeparator = false;
  for (let index = 0; index < value.length; index += 1) {
    const ch = value[index];
    if (/[:,.\-!?\s]/.test(ch)) {
      length = index + 1;
      sawSeparator = true;
      continue;
    }
    break;
  }
  return sawSeparator ? length : null;
}

function repairSentenceBoundaryBareCues(input: string, prepared: string, prep: PrepLike): string {
  const originalLower = String(input || "").toLowerCase();
  const phrases = barePerformanceCuePhrases(prep);
  let repaired = String(prepared || "");
  for (let attempt = 0; attempt < 8; attempt += 1) {
    let changed = false;
    outer: for (let index = 0; index < repaired.length; index += 1) {
      if (!isSentenceBoundary(repaired, index) || isInsideBracketTag(repaired, index)) continue;
      const rest = repaired.slice(index);
      for (const phrase of phrases) {
        if (originalLower.includes(phrase)) continue;
        const after = stripPrefixIgnoreCase(rest, phrase);
        if (after === null) continue;
        const afterLength = cueTrailingDelimiterLength(after);
        if (afterLength === null) continue;
        const candidate = `${repaired.slice(0, index)}[${phrase}] ${repaired
          .slice(index + phrase.length + afterLength)
          .trimStart()}`;
        if (!performanceTagsPreserveText(input, candidate)) continue;
        repaired = candidate;
        changed = true;
        break outer;
      }
    }
    if (!changed) break;
  }
  return repaired;
}

/**
 * Repair bare leading/sentence-boundary performance cues into bracketed tags.
 * Ports `repairBareLeadingPerformanceCue` (app.html line ~2306).
 */
export function repairBareLeadingPerformanceCue(
  input: string,
  prepared: string,
  prep: PrepLike,
): string {
  if (String(input || "").trim() === String(prepared || "").trim()) {
    return prepared;
  }
  return repairSentenceBoundaryBareCues(input, repairLeadingBareCue(input, prepared, prep), prep);
}

/**
 * Whether tagged output is valid: it must add bracket tags (unless unchanged)
 * and preserve the text. Ports `performanceTagsAreValid` (app.html line ~2312).
 */
export function performanceTagsAreValid(
  input: string | null | undefined,
  prepared: string | null | undefined,
): boolean {
  if (
    !bracketTags(prepared).length &&
    String(input || "").trim() !== String(prepared || "").trim()
  ) {
    return false;
  }
  return performanceTagsPreserveText(input, prepared);
}

const FALLBACK_TAG_CANDIDATES: [string, string[]][] = [
  ["whispers", ["whisper", "hushed", "under her breath", "under his breath"]],
  ["sigh of relief", ["relief", "relieved", "finally breathe", "safe at last"]],
  ["laughs", ["laugh", "laughed", "laughing"]],
  ["light chuckle", ["smile", "smiled", "grin", "amused"]],
  ["fearful", ["fear", "afraid", "terrified", "dread", "panic"]],
  ["nervous", ["tremor", "trembling", "anxious", "nervous"]],
  ["angry", ["angry", "furious", "rage", "outraged"]],
  ["sorrowful", ["sorrow", "grief", "tears", "wept", "crying", "mourning"]],
  ["wistful", ["remembered", "memory", "longed", "missed", "nostalgia"]],
  ["frustrated", ["frustrated", "irritated", "annoyed", "stuck"]],
  ["reassuring", ["safe", "steady", "promise", "trust", "breathe"]],
  [
    "tender",
    [
      "tender",
      "gentle",
      "soft",
      "carefully",
      "held",
      "kiss",
      "kisses",
      "kissing",
      "lips",
      "leans over",
    ],
  ],
  ["urgent", ["hurry", "urgent", "quickly", "now", "immediately"]],
  ["breathless", ["breathless", "gasped", "panting", "ran"]],
  ["proud", ["proud", "triumph", "victory", "accomplished"]],
  ["excited", ["excited", "thrilled", "delighted", "eager"]],
];

/** Pick a local fallback tag by keyword match. Ports `fallbackPerformanceTag`. */
export function fallbackPerformanceTag(
  input: string | null | undefined,
  prep: PrepLike,
): string | null {
  const palette = new Set((prep?.tagPalette || []).map((tag) => String(tag).toLowerCase()));
  const lower = String(input || "").toLowerCase();
  return (
    FALLBACK_TAG_CANDIDATES.find(
      ([tag, needles]) => palette.has(tag) && needles.some((needle) => lower.includes(needle)),
    )?.[0] || null
  );
}

function sentenceSegments(value: string): Array<{ start: number; end: number }> {
  const starts: number[] = [];
  const first = value.search(/\S/);
  if (first < 0) return [];
  starts.push(first);
  const boundaries = /[.!?](?:\s+|$)|\n+/g;
  for (const match of value.matchAll(boundaries)) {
    let start = (match.index ?? 0) + match[0].length;
    while (start < value.length && /\s/.test(value[start])) start += 1;
    if (start < value.length && starts[starts.length - 1] !== start) starts.push(start);
  }
  return starts.map((start, index) => ({ start, end: starts[index + 1] ?? value.length }));
}

/**
 * Build context-local fallback tags when remote prep fails. Returns `null`
 * unless the mode/strategy is inline-tags, the input is untagged, at least one
 * sentence matches the palette, and the result is within the provider limit.
 */
export function fallbackPerformanceTags(
  input: string | null | undefined,
  prep: PrepLike,
  strategy: string,
): string | null {
  if (prep?.mode !== "performance-tags" || strategy !== "inline-tags") return null;
  if (bracketTags(input).length) return null;
  const source = String(input || "");
  const insertions = sentenceSegments(source)
    .map(({ start, end }) => ({
      start,
      tag: fallbackPerformanceTag(source.slice(start, end), prep),
    }))
    .filter((entry): entry is { start: number; tag: string } => Boolean(entry.tag));
  if (!insertions.length) return null;
  let tagged = source;
  for (const { start, tag } of insertions.reverse()) {
    tagged = `${tagged.slice(0, start)}[${tag}] ${tagged.slice(start)}`;
  }
  if (Array.from(tagged).length > (prep?.maxLength ?? Infinity)) return null;
  if (!performanceTagsAreValid(input, tagged)) return null;
  return tagged;
}

/** Word-preservation ratio in `[0, 1]`. Ports `preservationRatio` (line ~2325). */
export function preservationRatio(
  input: string | null | undefined,
  prepared: string | null | undefined,
): number {
  const original = textWords(input);
  if (!original.length) return 1;
  const output = textWords(prepared);
  let found = 0;
  let outputIndex = 0;
  for (const word of original) {
    while (outputIndex < output.length && output[outputIndex] !== word) outputIndex += 1;
    if (outputIndex >= output.length) continue;
    found += 1;
    outputIndex += 1;
  }
  return found / original.length;
}

/**
 * Whether a style-instruction output is acceptable. Ports
 * `styleInstructionIsValid` (app.html line ~2312/…): ≤300 chars, no brackets or
 * code fences, not a labeled preamble, ≥3 words, and not a near-copy of the
 * input for longer inputs.
 */
export function styleInstructionIsValid(
  input: string | null | undefined,
  instruction: string | null | undefined,
): boolean {
  const trimmed = String(instruction || "").trim();
  if (Array.from(trimmed).length > 300) return false;
  if (trimmed.includes("[") || trimmed.includes("]") || trimmed.includes("```")) return false;
  if (/^(delivery instruction:|instruction:|here)/i.test(trimmed)) return false;
  if (textWords(trimmed).length < 3) return false;
  if (textWords(input).length >= 8 && preservationRatio(input, trimmed) > 0.45) return false;
  return true;
}
