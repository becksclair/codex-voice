/**
 * Shared types for the speech-prep pipeline.
 *
 * The prep subsystem derives a working "effective prep" object from
 * `config.speechPrep` (app.html's `browserSpeechPrepForDirect` and the
 * `speechPrepForProviderLimit`/`speechPrepForStreaming` transforms). Those
 * transforms add a runtime-only `forceSummarization` flag and may null out
 * `codexAuth`, so this widens {@link BrowserSpeechPrepConfig} accordingly.
 */

import type { BrowserCodexAuth, BrowserSpeechPrepConfig } from "../config.ts";

/**
 * A `config.speechPrep` object after the browser-direct transforms in
 * app.html (`browserSpeechPrepForDirect` and friends). `forceSummarization` is
 * set by {@link speechPrepForProviderLimit}; `codexAuth` may be `null` after a
 * Google fallback swap.
 */
export interface EffectiveSpeechPrep extends Omit<BrowserSpeechPrepConfig, "codexAuth"> {
  codexAuth?: BrowserCodexAuth | null;
  /** Runtime flag forcing summarization past the usual gates. */
  forceSummarization?: boolean;
}

/**
 * The subset of {@link WebSettings} the prep pipeline reads.
 *
 * `model` feeds provider-model resolution (strategy support checks);
 * `emotionPreprocessing`/`summarization` are the user toggles the decision tree
 * consults (app.html `prepareDecision`).
 */
export interface PrepSettings {
  model: string;
  emotionPreprocessing: boolean;
  summarization: boolean;
}

/**
 * Result of {@link prepareForProvider}.
 *
 * Field names/semantics mirror the object literals returned throughout
 * app.html's `prepareForProvider` (line ~2554). `changed` is the legacy name
 * for "the prepared `input` differs from the input this prep pass received"
 * (the task's `inputChanged`); callers that need "changed vs the original
 * request text" compute that separately, exactly as `synthesizeProvider` does.
 */
export interface PrepResult {
  /** Prepared text to synthesize (may equal the input). */
  input: string;
  /** Style-instruction output for providers that take delivery hints, else `null`. */
  instructions: string | null;
  /** Whether `input` differs from the text handed to this prep pass. */
  changed: boolean;
  /** Resolved strategy: `'shorten' | 'inline-tags' | 'style-instruction' | 'off'`. */
  strategy: string;
  /** Wall-clock milliseconds spent (0 when skipped). */
  elapsedMs: number;
  /** Set when prep was intentionally not run. */
  skipped?: boolean;
  /** Human-readable reason a prep pass was skipped. */
  reason?: string;
  /** User-facing error string when prep failed (input passes through raw). */
  error?: string;
  /** Warning string when a degraded fallback (local tag / excerpt) was used. */
  warning?: string;
  /** The prior shorten pass, when a performance pass ran on shortened text. */
  shortened?: PrepResult;
}

/** Options for {@link prepareForProvider}. */
export interface PrepareOptions {
  /** Prep cache shared across a generation run (keyed by provider/mode/limit/strategy/input). */
  prepCache?: Map<string, PrepResult> | null;
  /** Relax the performance-tags threshold to 0 for streaming (app.html `forcePerformanceTags`). */
  forcePerformanceTags?: boolean;
  /** Throw (instead of returning a skipped result) when browser prep is unavailable. */
  requireBrowserPrep?: boolean;
  /** Abort signal forwarded to prep fetches. */
  signal?: AbortSignal | null;
  /** Cancellation check; throws to abort (mirrors `throwIfGenerationCancelled`). */
  throwIfCancelled?: () => void;
  /** Status callback replacing legacy DOM status writes (unused by the port, reserved for B2). */
  onStatus?: (message: string) => void;
  /**
   * Invoked after a Codex OAuth refresh mutates `prep.codexAuth`, so callers can
   * re-persist the config (app.html re-writes `directConfig` to localStorage).
   * Receives the mutated effective prep.
   */
  onCodexAuthRefreshed?: (prep: EffectiveSpeechPrep) => void;
}
