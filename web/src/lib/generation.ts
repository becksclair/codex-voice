/**
 * Generation orchestration controller.
 *
 * Ports the run lifecycle and provider selection of app.html:
 * - persona/provider resolution (`selectedPersonaName`, `resolvePersona`,
 *   `resolveProvider`, `fallbackProvider`, `personaSupportsProvider`,
 *   `firstPersonaForProvider`) — lines ~955-1875
 * - `synthesizeProvider` / `generateDirect` / `generateViaServer` — lines
 *   ~3409-3559
 * - `runGeneration` and cancellation (`cancelActiveGeneration`,
 *   `throwIfGenerationCancelled`, `shouldKeepPendingGeneration`,
 *   `shouldApplyGeneratedText`) — lines ~1766-1835, ~3560-3660
 *
 * All DOM/global coupling becomes callbacks and injected accessors. The
 * decision tree, provider-fallback policy, persistence points, and user-facing
 * error strings are preserved exactly.
 */

import {
  streamAbortError,
  tryStreamProvider,
  type StreamState,
  type StreamingPlayback,
  type StreamingPlaybackCallbacks,
  type AudioContextConstructor,
} from "./audio/streaming.ts";
import { audioBlobFromBase64 } from "./audio/wav.ts";
import type { BrowserPersonaConfig, BrowserTtsConfig } from "./config.ts";
import { saveCachedConfig } from "./config.ts";
import { resolvePersona, resolveProvider } from "./personas.ts";
import { prepareForProvider, type PrepResult, type PrepSettings } from "./prep/index.ts";
import type { WebSettings } from "./settings.ts";
import {
  clearPendingGeneration,
  deleteLastGeneratedAudio,
  loadPendingGeneration,
  loadText,
  saveLastGeneratedAudio,
  savePendingGeneration,
  saveText,
  shouldApplyGeneratedText,
} from "./storage.ts";
import { canStreamElevenLabs, resolveElevenLabsModel } from "./synth/elevenlabs.ts";
import { synthesizeElevenLabs } from "./synth/elevenlabs.ts";
import { canStreamGoogle, resolveGoogleModel } from "./synth/google.ts";
import { synthesizeGoogle } from "./synth/google.ts";
import { cancelWebSpeechJob, createWebSpeechJob, waitForWebSpeechJob } from "./synth/serverJobs.ts";
import { clamp } from "./util.ts";

/** Error carrying an HTTP-ish status, as thrown across the pipeline. */
interface StatusError extends Error {
  status?: number;
  retryable?: boolean;
}

/** Metadata about a completed generation, passed to {@link GenerationCallbacks.onAudioReady}. */
export interface GenerationMeta {
  /** The text that was actually synthesized (may differ from the request input). */
  input: string;
  /** Whether the synthesized text differs from the original request input. */
  inputChanged: boolean;
  /** Provider that produced the audio: `'google' | 'elevenlabs' | 'server'`. */
  provider: string;
  /** True when the audio was produced by the streaming path. */
  streamed?: boolean;
  /** Streaming model id, when streamed. */
  streamingModel?: string;
  /**
   * Live streaming playback, when streamed. Callers should call
   * `playback.setReplayBlob(blob)` — the controller does this already — and wire
   * transport controls to it. Its drain fires `playbackCallbacks.onReplayReady`.
   */
  playback?: StreamingPlayback;
  /** Server job id, when produced via the server path. */
  jobId?: string;
}

/** Callbacks surfaced by {@link GenerationController}. */
export interface GenerationCallbacks {
  /** Progress label + fraction (mirrors `setGenerateProgress`/`setGenerating`). */
  onStatus?: (label: string, fraction: number) => void;
  /** Generation became active/idle (mirrors the `generate`/`clear` button state). */
  onGeneratingChange?: (active: boolean) => void;
  /** User-facing error string (mirrors `showError`). */
  onError?: (message: string) => void;
  /** Cleared error banner (mirrors `clearError`). */
  onClearError?: () => void;
  /** Final audio is ready (mirrors `loadAudioBlob` / the streamed replay wiring). */
  onAudioReady?: (blob: Blob, meta: GenerationMeta) => void;
  /** The draft text was replaced by the prepared text (mirrors `text.value = ...`). */
  onTextReplace?: (text: string) => void;
  /** High-level streaming state passthrough. */
  onStreamState?: (state: StreamState) => void;
  /** Forwarded to the streaming playback (position/peaks/replay). */
  playbackCallbacks?: StreamingPlaybackCallbacks;
}

/** Construction options for {@link GenerationController}. */
export interface GenerationControllerOptions {
  config: BrowserTtsConfig | null;
  settings: WebSettings;
  callbacks?: GenerationCallbacks;
  /** Current draft text accessor for `shouldApplyGeneratedText`; defaults to {@link loadText}. */
  getDraftText?: () => string;
  /** Injectable `AudioContext` resolver, forwarded to the streaming engine. */
  audioContextCtor?: () => AudioContextConstructor | null;
}

/** Internal per-provider synthesis result. */
interface SynthesisResult {
  blob: Blob;
  input: string;
  inputChanged: boolean;
  provider: string;
  prep?: PrepResult;
  streamed?: boolean;
  playback?: StreamingPlayback;
  streamingModel?: string;
  jobId?: string;
}

/** Immutable settings captured at the start of one generation run. */
type GenerationSettingsSnapshot = Readonly<WebSettings>;

function uniqueControllerId(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") return globalThis.crypto.randomUUID();
  const bytes = new Uint8Array(16);
  if (typeof globalThis.crypto?.getRandomValues === "function") {
    globalThis.crypto.getRandomValues(bytes);
    return Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

/** Whether an error is retryable for provider fallback. Ports `isRetryable`. */
export function isRetryable(error: StatusError | null | undefined): boolean {
  if (error?.retryable === false) return false;
  if (!error?.status) return true;
  return (
    error.status === 401 || error.status === 403 || error.status === 429 || error.status >= 500
  );
}

/** The other provider. Ports `fallbackProvider`. */
export function fallbackProvider(provider: string): string {
  return provider === "google" ? "elevenlabs" : "google";
}

/** Whether a provider supports streaming for the current config/persona/settings. Ports `canStreamProvider`. */
export function canStreamProvider(
  config: BrowserTtsConfig,
  provider: string,
  persona: BrowserPersonaConfig | null,
  settingsModel: string,
): boolean {
  if (provider === "elevenlabs") return canStreamElevenLabs(config, persona, settingsModel);
  if (provider === "google") return canStreamGoogle(config, settingsModel);
  return false;
}

/** Whether direct generation is possible. Ports `canGenerateDirectWithConfiguredPrep`. */
export function canGenerateDirectWithConfiguredPrep(
  config: BrowserTtsConfig | null | undefined,
): boolean {
  return Boolean(config?.providers?.google || config?.providers?.elevenlabs);
}

/** Whether settings match the server defaults (server fallback gate). Ports `settingsMatchServerDefaults`. */
export function settingsMatchServerDefaults(settings: WebSettings): boolean {
  return (
    settings.provider === "auto" &&
    settings.voice === "default" &&
    settings.model === "default" &&
    settings.emotionPreprocessing === true &&
    settings.summarization === false
  );
}

/**
 * Orchestrates a generation run: prep, provider selection, streaming/chunked/
 * server synthesis, persistence, and cancellation.
 */
export class GenerationController {
  private readonly controllerId = uniqueControllerId();
  private config: BrowserTtsConfig | null;
  private settings: WebSettings;
  private callbacks: GenerationCallbacks;
  private getDraftText: () => string;
  private audioContextCtorOption?: () => AudioContextConstructor | null;

  private runId = 0;
  private cancelled = false;
  private abortController: AbortController | null = null;
  private active = false;
  private activeStreamPlayback: StreamingPlayback | null = null;
  private lifecycleInterrupted = false;
  private activeServerJobId: string | null = null;

  constructor(options: GenerationControllerOptions) {
    this.config = options.config;
    this.settings = options.settings;
    this.callbacks = options.callbacks ?? {};
    this.getDraftText = options.getDraftText ?? (() => loadText());
    this.audioContextCtorOption = options.audioContextCtor;
  }

  /** Update the live config/settings/callbacks (e.g. after a config refresh). */
  update(
    patch: Partial<Pick<GenerationControllerOptions, "config" | "settings" | "callbacks">>,
  ): void {
    if (patch.config !== undefined) this.config = patch.config;
    if (patch.settings !== undefined) this.settings = patch.settings;
    if (patch.callbacks !== undefined) this.callbacks = patch.callbacks;
  }

  /** Whether a generation run is currently active. */
  get isActive(): boolean {
    return this.active;
  }

  /** The live streaming playback, if any. */
  get streamPlayback(): StreamingPlayback | null {
    return this.activeStreamPlayback;
  }

  /** Mark that an app-lifecycle event interrupted the run (pagehide/visibility). */
  markLifecycleInterrupted(): void {
    this.lifecycleInterrupted = true;
  }

  private status(fraction: number, label = "Generate"): void {
    this.callbacks.onStatus?.(label, clamp(Number(fraction) || 0, 0, 1));
  }

  private prepSettings(settings: GenerationSettingsSnapshot): PrepSettings {
    return {
      model: settings.model,
      emotionPreprocessing: settings.emotionPreprocessing,
      summarization: settings.summarization,
    };
  }

  private ensureNotCancelled(signal: AbortSignal | null, runId: number): void {
    if (signal?.aborted || this.cancelled || runId !== this.runId) throw streamAbortError();
  }

  private stopActiveStreamPlayback(): void {
    if (!this.activeStreamPlayback) return;
    const playback = this.activeStreamPlayback;
    this.activeStreamPlayback = null;
    playback.stop();
  }

  private releaseServerJob(jobId: string): void {
    if (this.activeServerJobId === jobId) this.activeServerJobId = null;
    void cancelWebSpeechJob(jobId).catch(() => {});
  }

  private shouldKeepPendingGeneration(error: StatusError | null | undefined): boolean {
    if (this.cancelled) return false;
    if (error?.status) return false;
    return (
      error?.name === "AbortError" || (this.lifecycleInterrupted && error?.name === "TypeError")
    );
  }

  /**
   * Cancel the active run: bump the run id, clear pending state, abort fetches,
   * and stop streaming playback. Ports `cancelActiveGeneration`.
   */
  cancel(): void {
    const serverJobId = this.activeServerJobId ?? loadPendingGeneration()?.jobId ?? null;
    this.cancelled = true;
    this.runId += 1;
    this.active = false;
    clearPendingGeneration();
    this.abortController?.abort();
    this.abortController = null;
    this.stopActiveStreamPlayback();
    this.activeServerJobId = null;
    if (serverJobId) this.releaseServerJob(serverJobId);
  }

  /** Synthesize with one provider (prep → stream or chunked). Ports `synthesizeProvider`. */
  private async synthesizeProvider(
    config: BrowserTtsConfig,
    provider: string,
    input: string,
    persona: BrowserPersonaConfig | null,
    prepCache: Map<string, PrepResult>,
    signal: AbortSignal | null,
    runId: number,
    settings: GenerationSettingsSnapshot,
  ): Promise<SynthesisResult> {
    this.status(0.32, "Preparing");
    const throwIfCancelled = (): void => this.ensureNotCancelled(signal, runId);
    const onCodexAuthRefreshed = (): void => {
      if (config) saveCachedConfig(config);
    };
    const forcePerformanceTags = canStreamProvider(config, provider, persona, settings.model);
    let prep = await prepareForProvider(
      config,
      provider,
      input,
      persona,
      this.prepSettings(settings),
      {
        prepCache,
        forcePerformanceTags,
        requireBrowserPrep: true,
        signal,
        throwIfCancelled,
        onCodexAuthRefreshed,
      },
    );
    if (prep.strategy === "shorten" && prep.input !== input) {
      const performancePrep = await prepareForProvider(
        config,
        provider,
        prep.input,
        persona,
        this.prepSettings(settings),
        {
          prepCache,
          forcePerformanceTags,
          requireBrowserPrep: true,
          signal,
          throwIfCancelled,
          onCodexAuthRefreshed,
        },
      );
      if (performancePrep.input !== prep.input || performancePrep.instructions) {
        prep = {
          ...performancePrep,
          shortened: prep,
          elapsedMs: (prep.elapsedMs || 0) + (performancePrep.elapsedMs || 0),
        };
      }
    }
    this.status(0.44, "Connecting");
    this.ensureNotCancelled(signal, runId);
    const streamOptions = {
      settingsModel: settings.model,
      signal,
      throwIfCancelled,
      onProgress: (fraction: number, label: string) => this.status(fraction, label),
      onStateChange: (state: StreamState) => this.callbacks.onStreamState?.(state),
      audioContextCtor: this.audioContextCtorOption,
      playbackCallbacks: this.callbacks.playbackCallbacks,
    };
    try {
      const streamed = await tryStreamProvider(
        config,
        provider,
        prep.input,
        persona,
        prep.instructions,
        streamOptions,
      );
      if (streamed) {
        return {
          blob: streamed.blob,
          input: prep.input,
          inputChanged: prep.input !== input,
          provider,
          prep,
          streamed: true,
          playback: streamed.playback,
          streamingModel: streamed.streamingModel,
        };
      }
    } catch (error) {
      console.warn(error);
      this.stopActiveStreamPlayback();
      this.status(0.58, "Fallback");
    }
    this.status(0.64, "Synthesizing");
    const model =
      provider === "google"
        ? resolveGoogleModel(config.providers?.google, settings.model)
        : resolveElevenLabsModel(config.providers?.elevenlabs, settings.model);
    const blob =
      provider === "google"
        ? await synthesizeGoogle(config, prep.input, persona, prep.instructions, {
            model,
            signal,
            throwIfCancelled,
          })
        : await synthesizeElevenLabs(config, prep.input, persona, {
            model,
            signal,
            throwIfCancelled,
          });
    return { blob, input: prep.input, inputChanged: prep.input !== input, provider, prep };
  }

  /** Direct (browser) generation with provider fallback. Ports `generateDirect`. */
  private async generateDirect(
    input: string,
    signal: AbortSignal | null,
    runId: number,
    settings: GenerationSettingsSnapshot,
  ): Promise<SynthesisResult> {
    const config = this.config as BrowserTtsConfig;
    const prepCache = new Map<string, PrepResult>();
    const selectedProvider = settings.provider !== "auto" ? settings.provider : null;
    const primary =
      selectedProvider || resolveProvider(config, resolvePersona(config, null, settings), settings);
    const persona = resolvePersona(config, primary, settings);
    try {
      return await this.synthesizeProvider(
        config,
        primary,
        input,
        persona,
        prepCache,
        signal,
        runId,
        settings,
      );
    } catch (error) {
      if (!isRetryable(error as StatusError) || persona?.fallbackPolicy !== "preserve-persona")
        throw error;
      const fallback = fallbackProvider(primary);
      if (!config.providers?.[fallback as "google" | "elevenlabs"]) throw error;
      return await this.synthesizeProvider(
        config,
        fallback,
        input,
        persona,
        prepCache,
        signal,
        runId,
        settings,
      );
    }
  }

  /** Server-job generation. Ports `generateViaServer`. */
  private async generateViaServer(
    input: string,
    jobId: string | null,
    signal: AbortSignal | null,
    runId: number,
    owner: string,
  ): Promise<SynthesisResult> {
    this.status(0.35, jobId ? "Resuming" : "Preparing");
    this.ensureNotCancelled(signal, runId);
    const activeJobId = jobId || (await createWebSpeechJob(input, signal));
    this.activeServerJobId = activeJobId;
    savePendingGeneration(input, activeJobId, owner);
    const result = await waitForWebSpeechJob(activeJobId, {
      signal,
      throwIfCancelled: () => this.ensureNotCancelled(signal, runId),
      onProgress: (fraction, label) => this.status(fraction, label),
    });
    return {
      blob: audioBlobFromBase64(result.audio_base64, result.mime_type),
      input: result.input,
      inputChanged: Boolean(result.input_changed),
      provider: "server",
      jobId: activeJobId,
    };
  }

  /**
   * Run a generation. Ports `runGeneration` (app.html line ~3560).
   *
   * Decision tree: resume a server job when `resumeJobId` is set; else if a
   * direct-capable config exists, try direct and fall back to the server path
   * only when settings match the server defaults; else go straight to the
   * server path.
   */
  async generate(input: string, resumeJobId: string | null = null): Promise<void> {
    const runSettings: GenerationSettingsSnapshot = Object.freeze({ ...this.settings });
    const runId = this.runId + 1;
    const runOwner = `${this.controllerId}:${runId}`;
    this.runId = runId;
    this.cancelled = false;
    const controller = new AbortController();
    this.abortController = controller;
    this.active = true;
    this.lifecycleInterrupted = false;
    this.stopActiveStreamPlayback();
    this.callbacks.onGeneratingChange?.(true);
    if (resumeJobId) savePendingGeneration(input, resumeJobId, runOwner);
    this.callbacks.onClearError?.();
    this.status(0.08, "Starting");
    let resumeAfterLifecycleInterruption = false;
    try {
      let result: SynthesisResult;
      if (resumeJobId) {
        result = await this.generateViaServer(
          input,
          resumeJobId,
          controller.signal,
          runId,
          runOwner,
        );
      } else if (this.config && canGenerateDirectWithConfiguredPrep(this.config)) {
        this.status(0.25, "Direct");
        try {
          result = await this.generateDirect(input, controller.signal, runId, runSettings);
        } catch (error) {
          if (!settingsMatchServerDefaults(runSettings)) throw error;
          this.status(0.25, "Server");
          result = await this.generateViaServer(input, null, controller.signal, runId, runOwner);
        }
      } else {
        this.status(0.25, "Server");
        result = await this.generateViaServer(input, null, controller.signal, runId, runOwner);
      }
      this.ensureNotCancelled(controller.signal, runId);
      const currentDraft = this.getDraftText();
      if (
        typeof result.input === "string" &&
        result.input !== currentDraft &&
        shouldApplyGeneratedText(currentDraft, input, result.input)
      ) {
        saveText(result.input);
        this.callbacks.onTextReplace?.(result.input);
      }
      this.status(0.9, "Saving");
      await saveLastGeneratedAudio(result.blob, result.input, result.inputChanged, runOwner);
      try {
        this.ensureNotCancelled(controller.signal, runId);
      } catch (error) {
        await deleteLastGeneratedAudio(runOwner);
        throw error;
      }
      if (result.jobId) this.releaseServerJob(result.jobId);
      const meta: GenerationMeta = {
        input: result.input,
        inputChanged: result.inputChanged,
        provider: result.provider,
        streamed: result.streamed,
        streamingModel: result.streamingModel,
        playback: result.playback,
        jobId: result.jobId,
      };
      if (result.streamed && result.playback) {
        this.activeStreamPlayback = result.playback;
        result.playback.setReplayBlob(result.blob);
      }
      this.callbacks.onAudioReady?.(result.blob, meta);
      clearPendingGeneration(runOwner);
      this.callbacks.onClearError?.();
      this.status(1, "Done");
    } catch (error) {
      const typed = error as StatusError;
      const cancelled = this.cancelled || controller.signal.aborted || runId !== this.runId;
      resumeAfterLifecycleInterruption = !cancelled && this.shouldKeepPendingGeneration(typed);
      if (!cancelled && !resumeAfterLifecycleInterruption && this.activeServerJobId) {
        this.releaseServerJob(this.activeServerJobId);
      }
      if (!resumeAfterLifecycleInterruption) clearPendingGeneration(runOwner);
      if (!cancelled && !resumeAfterLifecycleInterruption) {
        this.callbacks.onError?.(typed?.message || "TTS failed.");
      }
    } finally {
      if (this.abortController === controller) this.abortController = null;
      if (runId === this.runId) {
        this.active = false;
        this.callbacks.onGeneratingChange?.(false);
      }
    }
    if (
      resumeAfterLifecycleInterruption &&
      typeof document !== "undefined" &&
      document.visibilityState === "visible" &&
      loadPendingGeneration()
    ) {
      await this.resumePending();
    }
  }

  /** Resume a persisted server-job generation, if any. Ports `resumePendingGeneration`. */
  async resumePending(): Promise<boolean> {
    const pending = loadPendingGeneration();
    if (!pending || this.active) return false;
    const currentDraft = this.getDraftText();
    if (shouldApplyGeneratedText(currentDraft, pending.input, pending.input)) {
      saveText(pending.input);
      this.callbacks.onTextReplace?.(pending.input);
    }
    this.callbacks.onClearError?.();
    await this.generate(pending.input, pending.jobId || null);
    return true;
  }
}
