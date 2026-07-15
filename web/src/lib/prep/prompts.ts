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
  return `You are a careful literary compression editor preparing text for spoken performance. Produce a faithful condensed version of the text, not an abstract, analysis, outline, or commentary.\n\nHard length contract:\n- The final response must be no more than ${prep.maxLength} characters and should remain at least ${minLength} characters unless the source itself is shorter.\n- Use the available space. When the source exceeds the limit only modestly, make surgical cuts instead of rewriting or heavily compressing everything.\n- Silently check the length before responding and trim further if it is still over the maximum. Do not report the character count.\n\nPreservation priorities, in order:\n1. Preserve the complete semantic meaning: factual claims, names, decisions, requests, promises, constraints, chronology, causal links, relationships, negation, uncertainty, and who says or does what.\n2. Preserve the author's voice and point of view, including diction, cadence, paragraph flow, characterization, intimacy, humor, tension, subtext, and the emotional arc from beginning through the ending.\n3. Preserve distinctive imagery, metaphors, sensory and physical details, and memorable lines that carry tone or character. Do not replace specific language with generic summary language.\n4. Preserve intensity and ambiguity. Do not sanitize, moralize, euphemize, explain, resolve, or invent anything.\n\nCompression method:\n- Cut exact repetition, redundant restatement, low-value asides, boilerplate, formatting noise, URLs, and file paths first.\n- Compress transitions and repetitive lists before removing meaningful scenes, examples, images, dialogue, or emotional turns.\n- If code or markup appears, remove its formatting but retain any meaning needed by the surrounding text in concise spoken prose.\n- Keep the result coherent and natural when read aloud. Retain useful paragraph breaks.\n- Do not add bracketed performance tags; a separate pass handles delivery.\n- Return only the condensed text, with no markdown wrapper, preamble, label, explanation, or enclosing quotation marks.\n\nText:\n"""${input}"""`;
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
