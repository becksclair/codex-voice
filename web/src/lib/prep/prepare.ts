/**
 * `prepareForProvider` — the speech-prep entry point.
 *
 * Ports `prepareForProvider` from app.html (line ~2554) behavior-for-behavior:
 * the decision gate, cache, browser-supported vs server-only handling, the
 * per-model attempt loop with timeouts, tag/style validation and repair, the
 * shorten min-length excerpt fallback, and the failure/timeout local-tag
 * fallbacks. DOM status writes become the optional `onStatus` callback; the
 * `console.warn` diagnostics are preserved.
 */

import type { BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import { clamp } from "../util.ts";
import {
  browserSpeechPrepForDirect,
  prepareDecision,
  providerMaxTextLength,
  speechPrepForProviderLimit,
  speechPrepForStreaming,
  speechPrepStrategy,
  truncateToChars,
  extractiveShortenToFit,
} from "./decision.ts";
import {
  elapsedMs,
  extractTextOutput,
  fetchSpeechPrepAttempt,
  nonRetryableError,
  parseCodexSse,
  speechPrepAttemptTimeoutMs,
  speechPrepErrorIsRetryable,
  speechPrepModels,
  type PrepError,
} from "./codex.ts";
import {
  buildPerformanceTagsPrompt,
  buildShortenPrompt,
  buildStyleInstructionPrompt,
  performanceTagsOutputTokens,
  shortenMinOutputChars,
} from "./prompts.ts";
import {
  bracketTags,
  fallbackPerformanceTags,
  performanceTagsAreValid,
  repairBareLeadingPerformanceCue,
  styleInstructionIsValid,
} from "./tags.ts";
import type { EffectiveSpeechPrep, PrepareOptions, PrepResult, PrepSettings } from "./types.ts";

/**
 * Prepare `input` for a provider: emotion tagging, style instruction, or
 * summarization, per config and settings.
 *
 * @param config Live browser TTS config (its `speechPrep` drives the pipeline).
 * @param provider `'google' | 'elevenlabs'`.
 * @param input Raw text to prepare.
 * @param persona Resolved persona (delivery context for prompts), or `null`.
 * @param settings The user toggles the decision tree reads.
 * @param options Cache, cancellation, and callbacks.
 */
export async function prepareForProvider(
  config: BrowserTtsConfig,
  provider: string,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  settings: PrepSettings,
  options: PrepareOptions = {},
): Promise<PrepResult> {
  const prepCache = options.prepCache ?? null;
  const basePrep = browserSpeechPrepForDirect(config);
  const maxTextLength = providerMaxTextLength(config, provider);
  const mustShorten = Array.from(input).length > maxTextLength;
  const prep: EffectiveSpeechPrep | null = mustShorten
    ? speechPrepForProviderLimit(basePrep, maxTextLength)
    : options.forcePerformanceTags
      ? speechPrepForStreaming(basePrep)
      : basePrep;
  const strategy = speechPrepStrategy(
    { ...config, speechPrep: (prep ?? undefined) as BrowserTtsConfig["speechPrep"] },
    provider,
    settings.model,
  );
  const decision = prepareDecision(input, prep, strategy, settings);
  if (!decision.shouldPrepare) {
    return {
      input,
      instructions: null,
      changed: false,
      skipped: true,
      reason: decision.reason,
      strategy,
      elapsedMs: 0,
    };
  }
  const activePrep = prep as EffectiveSpeechPrep;
  const cacheKey = `${activePrep.provider}\n${activePrep.mode}\n${activePrep.maxLength}\n${strategy}\n${input}`;
  const cached = prepCache?.get(cacheKey);
  if (cached) return cached;

  const remember = (result: PrepResult): PrepResult => {
    prepCache?.set(cacheKey, result);
    return result;
  };

  if (!activePrep.browserSupported) {
    const message =
      activePrep.mode === "shorten"
        ? "Configured summarization prep is server-only."
        : "Configured emotion prep is server-only.";
    if (options.requireBrowserPrep) throw nonRetryableError(message);
    return remember({
      input,
      instructions: null,
      changed: false,
      skipped: true,
      error: message,
      strategy,
      elapsedMs: 0,
    });
  }
  if (activePrep.provider === "google" && !activePrep.apiKey) {
    throw new Error("Google emotion prep is missing an API key.");
  }
  if (activePrep.provider === "codex" && !activePrep.codexAuth?.accessToken) {
    throw new Error("Codex emotion prep is missing cached auth.");
  }

  const onRefreshed = (refreshed: EffectiveSpeechPrep): void => {
    options.onCodexAuthRefreshed?.(refreshed);
  };

  const startedAt = performance.now();
  const prepInput =
    activePrep.mode === "shorten"
      ? truncateToChars(input, Number(activePrep.maxInputLength) || Infinity)
      : input;
  const prompt =
    activePrep.mode === "shorten"
      ? buildShortenPrompt(prepInput, activePrep)
      : strategy === "style-instruction"
        ? buildStyleInstructionPrompt(prepInput, activePrep, persona)
        : buildPerformanceTagsPrompt(prepInput, activePrep, persona);
  const body = {
    contents: [{ role: "user", parts: [{ text: prompt }] }],
    generationConfig: {
      temperature: activePrep.mode === "shorten" ? 0.2 : 0.45,
      maxOutputTokens:
        activePrep.mode === "shorten"
          ? clamp(Math.floor(activePrep.maxLength / 3), 64, 4096)
          : strategy === "style-instruction"
            ? 128
            : performanceTagsOutputTokens(prepInput, activePrep),
      ...(activePrep.mode === "performance-tags"
        ? { thinkingConfig: { thinkingLevel: "MINIMAL" } }
        : {}),
    },
  };
  const overallTimeoutMs = Number(activePrep.timeoutMs) || 30000;
  const attemptTimeoutMs = speechPrepAttemptTimeoutMs(activePrep);
  let lastError: PrepError | null = null;

  const localTagFallback = (warning: string): PrepResult | null => {
    const fallback = fallbackPerformanceTags(input, activePrep, strategy);
    if (!fallback) return null;
    return remember({
      input: fallback,
      instructions: null,
      changed: true,
      warning,
      strategy,
      elapsedMs: elapsedMs(startedAt),
    });
  };
  const fallbackOrPassThrough = (message: string): PrepResult =>
    localTagFallback(message) ||
    remember({
      input,
      instructions: null,
      changed: false,
      error: message,
      strategy,
      elapsedMs: elapsedMs(startedAt),
    });

  try {
    for (const model of speechPrepModels(activePrep)) {
      options.throwIfCancelled?.();
      const remainingMs = overallTimeoutMs - elapsedMs(startedAt);
      if (remainingMs <= 0) break;
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), Math.min(attemptTimeoutMs, remainingMs));
      // Chain the caller's cancellation signal into this attempt so cancelling
      // a run aborts an in-flight prep request instead of letting it complete
      // wastefully (cancellation is otherwise only observed at the loop top).
      const externalSignal = options.signal ?? null;
      const onExternalAbort = (): void => controller.abort();
      if (externalSignal?.aborted) controller.abort();
      else externalSignal?.addEventListener("abort", onExternalAbort, { once: true });
      try {
        const response = await fetchSpeechPrepAttempt(
          activePrep,
          model,
          body,
          prompt,
          controller.signal,
          onRefreshed,
        );
        if (!response.ok) {
          const error = (await providerPrepError(response, "Emotion prep failed")) as PrepError;
          lastError = error;
          console.warn(error.message);
          if (speechPrepErrorIsRetryable(error)) continue;
          return fallbackOrPassThrough(error.message);
        }
        let prepared =
          activePrep.provider === "codex"
            ? parseCodexSse(await response.text()).trim()
            : extractTextOutput(await response.json()).trim();
        if (!prepared) {
          return fallbackOrPassThrough("Emotion prep returned no text.");
        }
        if (activePrep.mode === "performance-tags" && strategy === "inline-tags") {
          prepared = repairBareLeadingPerformanceCue(input, prepared, activePrep);
        }
        if (
          activePrep.mode === "performance-tags" &&
          strategy === "inline-tags" &&
          Array.from(prepared).length > activePrep.maxLength
        ) {
          return fallbackOrPassThrough("Emotion prep returned text above the configured limit.");
        }
        if (
          activePrep.mode === "performance-tags" &&
          strategy === "inline-tags" &&
          !performanceTagsAreValid(input, prepared)
        ) {
          return fallbackOrPassThrough(
            "Emotion prep changed the text too much, so local performance tags were used.",
          );
        }
        if (
          activePrep.mode === "performance-tags" &&
          strategy === "style-instruction" &&
          !styleInstructionIsValid(input, prepared)
        ) {
          return remember({
            input,
            instructions: null,
            changed: false,
            error: "Emotion prep returned an invalid delivery instruction, so it was ignored.",
            strategy,
            elapsedMs: elapsedMs(startedAt),
          });
        }
        const output =
          activePrep.mode === "shorten"
            ? Array.from(prepared).slice(0, activePrep.maxLength).join("")
            : prepared;
        if (
          activePrep.mode === "shorten" &&
          Array.from(output).length < shortenMinOutputChars(input, activePrep)
        ) {
          const extracted = extractiveShortenToFit(prepInput, activePrep.maxLength);
          return remember({
            input: extracted,
            instructions: null,
            changed: extracted !== input,
            warning:
              "Summarization returned text below the minimum length, so a fitted source excerpt was used.",
            strategy,
            elapsedMs: elapsedMs(startedAt),
          });
        }
        if (strategy === "style-instruction") {
          return remember({
            input,
            instructions: output,
            changed: false,
            strategy,
            elapsedMs: elapsedMs(startedAt),
          });
        }
        if (activePrep.mode === "performance-tags" && strategy === "inline-tags") {
          const local = fallbackPerformanceTags(input, activePrep, strategy);
          const remoteTagCount = bracketTags(output).length;
          if (local && remoteTagCount <= 2 && bracketTags(local).length > remoteTagCount) {
            return remember({
              input: local,
              instructions: null,
              changed: true,
              warning: "Local emotion coverage added cues missed by remote prep.",
              strategy,
              elapsedMs: elapsedMs(startedAt),
            });
          }
        }
        return remember({
          input: output,
          instructions: null,
          changed: output !== input,
          strategy,
          elapsedMs: elapsedMs(startedAt),
        });
      } catch (error) {
        const typed = error as PrepError;
        if (activePrep.provider === "codex" && typed?.name === "TypeError") {
          typed.message = "Codex direct emotion prep is blocked by the browser or network.";
        }
        lastError = typed;
        if (speechPrepErrorIsRetryable(typed)) continue;
        return fallbackOrPassThrough(typed?.message || "Emotion prep failed.");
      } finally {
        clearTimeout(timer);
        externalSignal?.removeEventListener("abort", onExternalAbort);
      }
    }
    if (lastError) {
      const fallback = localTagFallback(
        lastError?.message || "Emotion prep failed, so local performance tags were used.",
      );
      if (fallback) return fallback;
      return remember({
        input,
        instructions: null,
        changed: false,
        error: lastError?.message || "Emotion prep failed after retries.",
        strategy,
        elapsedMs: elapsedMs(startedAt),
      });
    }
    const timeoutFallback = localTagFallback(
      "Emotion prep timed out, so local performance tags were used.",
    );
    if (timeoutFallback) return timeoutFallback;
    return remember({
      input,
      instructions: null,
      changed: false,
      error: "Emotion prep timed out before a model returned text.",
      strategy,
      elapsedMs: elapsedMs(startedAt),
    });
  } catch (error) {
    const typed = error as PrepError;
    console.warn(typed);
    const fallback = localTagFallback(
      typed?.message || "Emotion prep failed, so a local sparse performance tag was used.",
    );
    if (fallback) return fallback;
    return remember({
      input,
      instructions: null,
      changed: false,
      error: typed?.message || "Emotion prep failed.",
      strategy,
      elapsedMs: elapsedMs(startedAt),
    });
  }
}

async function providerPrepError(response: Response, fallback: string): Promise<PrepError> {
  let text = "";
  try {
    text = await response.text();
  } catch {
    // Ignored, matching app.html behavior.
  }
  const error = new Error(
    text ? `${fallback}: ${text}` : `${fallback} (${response.status})`,
  ) as PrepError;
  error.status = response.status;
  return error;
}
