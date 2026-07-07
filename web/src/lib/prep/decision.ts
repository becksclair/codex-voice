/**
 * Speech-prep decision tree and config transforms.
 *
 * Ports from app.html (lines ~1892-2035):
 * - provider/model helpers used by strategy selection
 *   (`providerSupportsInlineAudioTags`, `googleSupportsStyleInstruction`)
 * - `speechPrepStrategy`
 * - `browserSpeechPrepForDirect` / `googleSpeechPrepFallback`
 * - `speechPrepForProviderLimit` / `speechPrepForStreaming` / `shortenFitLimit`
 * - `truncateToChars` / `extractiveShortenToFit`
 * - `providerMaxTextLength`
 * - `prepareDecision` / `shouldPrepare`
 */

import type { BrowserTtsConfig } from "../config.ts";
import { resolveElevenLabsModel } from "../synth/elevenlabs.ts";
import { resolveGoogleModel } from "../synth/google.ts";
import { MIN_SHORTEN_OUTPUT_CHARS, shortenPrepareFloor } from "./prompts.ts";
import type { EffectiveSpeechPrep, PrepSettings } from "./types.ts";

/** Result of {@link prepareDecision}. */
export interface PrepareDecision {
  shouldPrepare: boolean;
  reason: string;
}

/**
 * Whether a provider supports inline audio tags. Ports
 * `providerSupportsInlineAudioTags` (app.html line ~1902). `settingsModel`
 * feeds model resolution.
 */
export function providerSupportsInlineAudioTags(
  config: BrowserTtsConfig,
  provider: string,
  settingsModel?: string | null,
): boolean {
  if (provider === "google") {
    const google = config.providers?.google;
    if (!google) return false;
    if (typeof google.inlineAudioTags === "boolean") return google.inlineAudioTags;
    const model = resolveGoogleModel(google, settingsModel).toLowerCase();
    return model.includes("gemini-3.1") && model.includes("tts");
  }
  if (provider === "elevenlabs") {
    const elevenlabs = config.providers?.elevenlabs;
    if (!elevenlabs) return false;
    if (typeof elevenlabs.inlineAudioTags === "boolean") return elevenlabs.inlineAudioTags;
    const model = resolveElevenLabsModel(elevenlabs, settingsModel).toLowerCase();
    return model === "eleven_v3" || model.startsWith("eleven_v3_");
  }
  return false;
}

/** Whether Google supports a style instruction. Ports `googleSupportsStyleInstruction`. */
export function googleSupportsStyleInstruction(
  config: BrowserTtsConfig,
  settingsModel?: string | null,
): boolean {
  const model = resolveGoogleModel(config.providers?.google, settingsModel).toLowerCase();
  return model.includes("gemini") && model.includes("tts");
}

/**
 * Resolve the prep strategy for a provider. Ports `speechPrepStrategy`
 * (app.html line ~1925). Returns `'shorten' | 'inline-tags' |
 * 'style-instruction' | 'off'`.
 */
export function speechPrepStrategy(
  config: BrowserTtsConfig,
  provider: string,
  settingsModel?: string | null,
): string {
  const prep = config?.speechPrep;
  if (!prep || prep.mode === "shorten") return "shorten";
  const configured =
    provider === "google"
      ? prep.strategies?.google
      : provider === "elevenlabs"
        ? prep.strategies?.elevenlabs
        : prep.strategies?.default;
  const strategy =
    configured && configured !== "off" ? configured : prep.strategies?.default || "off";
  if (strategy === "inline-tags") {
    return providerSupportsInlineAudioTags(config, provider, settingsModel) ? "inline-tags" : "off";
  }
  if (strategy === "style-instruction") {
    return provider === "google" && googleSupportsStyleInstruction(config, settingsModel)
      ? "style-instruction"
      : "off";
  }
  return "off";
}

/** Build a Google fallback prep from `browserFallback`. Ports `googleSpeechPrepFallback`. */
export function googleSpeechPrepFallback(
  prep: EffectiveSpeechPrep | null | undefined,
): EffectiveSpeechPrep | null {
  const fallback = prep?.browserFallback;
  if (fallback?.provider !== "google" || !fallback.apiKey || !fallback.baseUrl || !fallback.model) {
    return null;
  }
  return {
    ...(prep as EffectiveSpeechPrep),
    provider: "google",
    browserSupported: true,
    apiKey: fallback.apiKey,
    codexAuth: null,
    baseUrl: fallback.baseUrl,
    model: fallback.model,
    fallbackModels: fallback.fallbackModels || [],
    reasoningEffort: undefined,
  };
}

/**
 * Resolve the effective browser-direct prep. Ports `browserSpeechPrepForDirect`
 * (app.html line ~1961). Server-only prep swaps to a Google fallback when one
 * is configured, otherwise passes through unchanged.
 */
export function browserSpeechPrepForDirect(
  config: BrowserTtsConfig | null | undefined,
): EffectiveSpeechPrep | null {
  const prep = config?.speechPrep as EffectiveSpeechPrep | undefined;
  if (!prep || prep.browserSupported !== false) return prep || null;
  return googleSpeechPrepFallback(prep) || prep;
}

/** Clamp a shorten target to the provider max. Ports `shortenFitLimit`. */
export function shortenFitLimit(providerMaxLength: number): number {
  if (!Number.isFinite(providerMaxLength) || providerMaxLength <= MIN_SHORTEN_OUTPUT_CHARS) {
    return providerMaxLength;
  }
  return MIN_SHORTEN_OUTPUT_CHARS;
}

/** Force a shorten prep sized to the provider limit. Ports `speechPrepForProviderLimit`. */
export function speechPrepForProviderLimit(
  prep: EffectiveSpeechPrep | null,
  maxLength: number,
): EffectiveSpeechPrep | null {
  if (!prep || !Number.isFinite(maxLength)) return prep;
  const targetLength = shortenFitLimit(maxLength);
  return {
    ...prep,
    mode: "shorten",
    maxLength: targetLength,
    threshold: Math.min(targetLength, MIN_SHORTEN_OUTPUT_CHARS),
    forceSummarization: true,
  };
}

/** Drop the threshold for streaming performance-tag prep. Ports `speechPrepForStreaming`. */
export function speechPrepForStreaming(
  prep: EffectiveSpeechPrep | null,
): EffectiveSpeechPrep | null {
  if (!prep || prep.mode !== "performance-tags") return prep;
  return { ...prep, threshold: 0 };
}

/** Truncate to a codepoint count. Ports `truncateToChars` (app.html line ~1991). */
export function truncateToChars(value: string, maxLength: number): string {
  if (!Number.isFinite(maxLength)) return value;
  const chars = Array.from(value);
  return chars.length <= maxLength ? value : chars.slice(0, maxLength).join("");
}

/** Extractive shorten (currently a truncation). Ports `extractiveShortenToFit`. */
export function extractiveShortenToFit(value: string, maxLength: number): string {
  return truncateToChars(value, maxLength);
}

/** Provider max text length, falling back to config max then Infinity. Ports `providerMaxTextLength`. */
export function providerMaxTextLength(
  config: BrowserTtsConfig | null | undefined,
  provider: string,
): number {
  const providerConfig = config?.providers?.[provider as "google" | "elevenlabs"];
  return Number(providerConfig?.maxTextLength) || Number(config?.maxTextLength) || Infinity;
}

/**
 * Decide whether to run prep. Ports `prepareDecision` (app.html line ~2001).
 * The `settings` toggles gate performance-tags/shorten modes exactly as the
 * legacy `settings.emotionPreprocessing`/`settings.summarization` did.
 */
export function prepareDecision(
  input: string,
  prep: EffectiveSpeechPrep | null | undefined,
  strategy: string,
  settings: PrepSettings,
): PrepareDecision {
  if (!prep) return { shouldPrepare: false, reason: "No speech prep config." };
  if (prep.mode === "performance-tags" && !settings.emotionPreprocessing) {
    return { shouldPrepare: false, reason: "Emotion prep is off." };
  }
  if (prep.mode === "shorten" && !settings.summarization && !prep.forceSummarization) {
    return { shouldPrepare: false, reason: "Summarization is off." };
  }
  if (prep.mode !== "performance-tags" && prep.mode !== "shorten") {
    return { shouldPrepare: false, reason: "Unsupported speech prep mode." };
  }
  if (prep.mode === "performance-tags" && strategy === "off") {
    return {
      shouldPrepare: false,
      reason: "Speech model does not support configured emotion prep.",
    };
  }
  const chars = Array.from(input).length;
  if (chars < prep.threshold) {
    return { shouldPrepare: false, reason: "Text is below the prep threshold." };
  }
  if (chars > prep.maxInputLength && !prep.forceSummarization) {
    return { shouldPrepare: false, reason: "Text is too long for prep." };
  }
  if (prep.mode === "shorten" && chars <= shortenPrepareFloor(prep)) {
    return { shouldPrepare: false, reason: "Text already fits without summarization." };
  }
  if (prep.mode === "shorten" && chars <= prep.maxLength) {
    return { shouldPrepare: false, reason: "Text already fits the speech limit." };
  }
  return { shouldPrepare: true, reason: "" };
}

/** Convenience predicate. Ports `shouldPrepare` (app.html line ~2027). */
export function shouldPrepare(
  input: string,
  prep: EffectiveSpeechPrep | null | undefined,
  supportsInlineAudioTags: boolean,
  settings: PrepSettings,
): boolean {
  return prepareDecision(input, prep, supportsInlineAudioTags ? "inline-tags" : "off", settings)
    .shouldPrepare;
}
