/**
 * Prompt construction and token/length math for speech prep.
 *
 * Ports the prep constants and prompt builders from app.html (lines
 * ~777-780, ~2036-2095): `performanceTagsOutputTokens`, `buildShortenPrompt`,
 * `shortenPrepareFloor`, `shortenMinOutputChars`, `buildPerformanceTagsPrompt`,
 * `buildStyleInstructionPrompt`.
 */

import type { BrowserPersonaConfig } from "../config.ts";
import { clamp } from "../util.ts";
import type { EffectiveSpeechPrep } from "./types.ts";

/** Default per-attempt prep timeout (ms). Ports `defaultSpeechPrepAttemptTimeoutMs`. */
export const DEFAULT_SPEECH_PREP_ATTEMPT_TIMEOUT_MS = 4000;
/** Capped output-token ceiling for performance tags. Ports `performanceTagsMaxOutputTokens`. */
export const PERFORMANCE_TAGS_MAX_OUTPUT_TOKENS = 384;
/** Uncapped output-token ceiling for performance tags. Ports `performanceTagsAbsoluteMaxOutputTokens`. */
export const PERFORMANCE_TAGS_ABSOLUTE_MAX_OUTPUT_TOKENS = 4096;
/** Character floor below which shortening is skipped. Ports `minShortenOutputChars`. */
export const MIN_SHORTEN_OUTPUT_CHARS = 4000;

/** Default performance-tag palette when `prep.tagPalette` is empty. */
export const DEFAULT_TAG_PALETTE = [
  "excited",
  "delighted",
  "playful",
  "brightly",
  "nervous",
  "uneasy",
  "fearful",
  "frustrated",
  "angry",
  "stern",
  "sorrowful",
  "wistful",
  "choked up",
  "calm",
  "reassuring",
  "tender",
  "vulnerable",
  "affectionate",
  "proud",
  "determined",
  "amused",
  "dryly",
  "deadpan",
  "relieved",
  "sleepy",
  "serious",
  "urgent",
  "teasing",
  "warmly",
  "softly",
  "flatly",
  "breathless",
  "sigh",
  "laughs",
  "laughing",
  "gasps",
  "whispers",
  "exhales",
  "shaky breath",
  "light chuckle",
  "snorts",
  "scoffs",
  "sigh of relief",
  "hesitates",
  "pause",
  "long pause",
  "voice breaks",
  "swallows",
  "leans closer",
  "under breath",
  "smiling",
  "moan",
];

/** Compute the performance-tags output-token budget. Ports `performanceTagsOutputTokens`. */
export function performanceTagsOutputTokens(input: string, prep: EffectiveSpeechPrep): number {
  const inputChars = Array.from(input).length;
  const maxDefaultTokens = prep?.capPerformanceTags
    ? PERFORMANCE_TAGS_MAX_OUTPUT_TOKENS
    : PERFORMANCE_TAGS_ABSOLUTE_MAX_OUTPUT_TOKENS;
  const defaultCap = clamp(Math.floor(prep.maxLength / 2), 128, maxDefaultTokens);
  const preserveCap = clamp(
    Math.floor(inputChars / 3),
    128,
    PERFORMANCE_TAGS_ABSOLUTE_MAX_OUTPUT_TOKENS,
  );
  return Math.max(defaultCap, preserveCap);
}

/** Character floor below which a shorten pass is a no-op. Ports `shortenPrepareFloor`. */
export function shortenPrepareFloor(prep: EffectiveSpeechPrep): number {
  return Math.max(
    Number(prep.threshold) || 0,
    Math.min(MIN_SHORTEN_OUTPUT_CHARS, Number(prep.maxLength) || MIN_SHORTEN_OUTPUT_CHARS),
  );
}

/** Minimum acceptable shortened-output length. Ports `shortenMinOutputChars`. */
export function shortenMinOutputChars(input: string, prep: EffectiveSpeechPrep): number {
  const inputChars = Array.from(input).length;
  return Math.min(inputChars, Number(prep.maxLength) || inputChars, MIN_SHORTEN_OUTPUT_CHARS);
}

/** Build the shorten/summarize prompt. Ports `buildShortenPrompt` (line ~2044). */
export function buildShortenPrompt(input: string, prep: EffectiveSpeechPrep): string {
  const minLength = shortenMinOutputChars(input, prep);
  return `Prepare this text for text-to-speech playback. Preserve the user's meaning, key facts, decisions, and the full requested message. Shorten only when necessary to stay under ${prep.maxLength} characters. Keep the prepared text at least ${minLength} characters unless the source text itself is shorter. Do not collapse prose into a short abstract. Remove repetition, code blocks, URLs, file paths, and formatting noise. Return only natural speakable prose, no markdown, no preamble, no labels.\n\nText:\n"""${input}"""`;
}

function personaDeliveryContext(persona: BrowserPersonaConfig | null | undefined): string {
  if (!persona) return "";
  let prompt = "Delivery context:\n";
  prompt += `- persona: ${persona.label} - ${persona.description}\n`;
  if (persona.promptScene) prompt += `- scene: ${persona.promptScene}\n`;
  if (persona.promptStyle) prompt += `- style: ${persona.promptStyle}\n`;
  if (persona.promptPacing) prompt += `- pace: ${persona.promptPacing}\n`;
  for (const constraint of persona.promptConstraints || []) {
    prompt += `- constraint: ${constraint}\n`;
  }
  prompt += "\n";
  return prompt;
}

/** Build the inline performance-tags prompt. Ports `buildPerformanceTagsPrompt`. */
export function buildPerformanceTagsPrompt(
  input: string,
  prep: EffectiveSpeechPrep,
  persona: BrowserPersonaConfig | null | undefined,
): string {
  let prompt =
    "You are a TTS performance tagger. Do not rewrite the text. Do not summarize, omit, or reorder it. Build a coherent performance arc by inserting concise emotion or delivery tags at meaningful changes in emotional state, pacing, tension, realization, or physical performance. Choose the most textually supported cue: distinguish dread, shock, revulsion, grief, irony, urgency, and tenderness instead of substituting a generic mood. Do not invent sorrow, urgency, bitterness, humor, or physical reactions unless the words support them. Keep each cue local to the complete sentence or clause it governs. Place each tag immediately before that sentence or clause, never between a determiner and its noun or inside a fixed phrase. Follow every closing bracket with exactly one space. Never place tags back-to-back; combine compatible direction into one concise bracketed cue when necessary. Do not impose an arbitrary limit on the number of tags; cover the emotional progression throughout the text, but avoid redundant cues where delivery does not change. For emotionally charged prose longer than 800 characters, sustain cues through the final emotionally active sentence and use roughly one meaningful cue per 80-140 characters as coverage guidance, not as a minimum or maximum count. Do not stop tagging merely because the opening and climax have cues. Prefer performable direction over literary analysis. Never add a cue that contradicts the text. Return only the tagged text, with no enclosing quotation marks, code fence, label, or delimiter. Every cue must be enclosed in square brackets, like [softly] or [gasps, horrified]. If the text is genuinely neutral and no cue improves delivery, return it unchanged.\n";
  prompt +=
    "Semantic distinction: reserve sorrow and grief for actual loss, mourning, tears, or regret. Grotesque imagery, bodily horror, and fearful disgust call for dread, horror, or revulsion instead of sorrow.\n";
  // Matches the original `prep.tagPalette || DEFAULT_TAG_PALETTE`: only a
  // nullish palette falls back; an empty array yields an empty palette list.
  const palette = (prep.tagPalette || DEFAULT_TAG_PALETTE).map((tag) => `[${tag}]`).join(", ");
  prompt += `Use inline bracketed audio tags from this palette when they fit: ${palette}. Closely related performable cues are allowed when the palette does not fit, but they must also be square-bracketed. Keep the result under `;
  prompt += `${prep.maxLength} characters.\n\n`;
  prompt += personaDeliveryContext(persona);
  prompt += `Text:\n"""${input}"""`;
  return prompt;
}

/** Build the style-instruction prompt. Ports `buildStyleInstructionPrompt`. */
export function buildStyleInstructionPrompt(
  input: string,
  _prep: EffectiveSpeechPrep,
  persona: BrowserPersonaConfig | null | undefined,
): string {
  let prompt =
    "You are a TTS delivery director for Google Gemini speech synthesis. Do not rewrite, summarize, quote, or repeat the text. Return only a 1-3 sentence natural-language delivery instruction for how the voice should perform this exact message: emotional state, pacing, intimacy, tension, hesitation, warmth, and release. Keep it concrete and speakable as direction, not content. Never include bracket tags. Keep the instruction under 300 characters.\n\n";
  prompt += personaDeliveryContext(persona);
  prompt += `Text to direct, not rewrite:\n"""${input}"""`;
  return prompt;
}
