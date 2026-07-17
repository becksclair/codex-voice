/**
 * Streaming TTS playback engine.
 *
 * Ports the streaming path of app.html (lines ~1511-1683, ~2936-3242) as a
 * headless, DOM-free engine:
 * - {@link StreamingPlayback}: interleaves incremental PCM decode with
 *   `AudioContext` scheduling, seek, pause/resume, and drain-to-replay.
 * - {@link createPcmStreamSink}: buffers odd bytes, applies gain, feeds the
 *   playback, and stitches a final WAV blob.
 * - {@link streamGoogle}/{@link readGoogleInteractionStream} and
 *   {@link streamElevenLabs}/{@link streamElevenLabsHttp}: read the network
 *   streams and drive the sink.
 *
 * All DOM/global coupling (`activeStreamPlayback`, canvas waveform, transport
 * button state, `setGenerateProgress`) is replaced by injectable callbacks. The
 * `AudioContext` constructor is injectable so tests can supply a fake. Request
 * shapes, headers, and SSE framing are faithful to the live API.
 */

import type { BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import {
  applyPcm16Gain,
  evenPcmBytes,
  pcm16ToAudioBuffer,
  ttsStreamPcmGain,
  type PendingByte,
} from "./pcm.ts";
import { concatUint8Arrays, wavBlobFromPcm } from "./wav.ts";
import { bytesFromBase64 } from "./wav.ts";
import { audioContextCtor, streamingPcmPeaks } from "./waveform.ts";
import { clamp } from "../util.ts";
import {
  canStreamElevenLabs,
  elevenLabsSampleRate,
  resolveElevenLabsSpeed,
  resolveElevenLabsStreamingModel,
  elevenLabsWebSocketModelSupported,
  websocketBaseUrl,
} from "../synth/elevenlabs.ts";
import { providerTimeoutSignal } from "../synth/timeout.ts";
import {
  buildGoogleTtsPrompt,
  canStreamGoogle,
  normalizeGoogleModelName,
  resolveGoogleModel,
} from "../synth/google.ts";
import { providerError } from "../synth/common.ts";

/** `AudioContext` constructor shape (no-arg, as used by the engine). */
export interface AudioContextConstructor {
  new (): AudioContext;
}

/** High-level streaming state surfaced to the UI. */
export type StreamState = "buffering" | "playing" | "done" | "error";

/**
 * Callbacks for {@link StreamingPlayback}. Every DOM write in the legacy class
 * maps to one of these; all are optional.
 */
export interface StreamingPlaybackCallbacks {
  /** Position tick: current seconds, estimated total, and whether the stream ended. */
  onProgress?: (currentSeconds: number, estimatedDuration: number, finished: boolean) => void;
  /** New waveform peaks for an appended PCM chunk, plus its duration in seconds. */
  onPeaks?: (peaks: number[], durationDelta: number) => void;
  /** Play/pause state (mirrors the legacy `playSvg`). `true` = playing. */
  onPlayingChange?: (playing: boolean) => void;
  /** The stream reached its end (all audio received). */
  onFinished?: () => void;
  /** Playback fully drained; the complete blob is ready to load as fixed audio. */
  onReplayReady?: (blob: Blob) => void;
}

/** Options for {@link StreamingPlayback}. */
export interface StreamingPlaybackOptions {
  /** Injectable `AudioContext` constructor resolver; defaults to {@link audioContextCtor}. */
  audioContextCtor?: () => AudioContextConstructor | null;
  callbacks?: StreamingPlaybackCallbacks;
}

interface PcmChunk {
  bytes: Uint8Array;
  sampleRate: number;
  channels: number;
  duration: number;
}

/**
 * Incremental PCM playback over a Web Audio `AudioContext`.
 *
 * Ports the `StreamingPlayback` class (app.html line ~1511). Behavior — buffer
 * scheduling with an 80ms lead and a 30ms floor, seek by rebuilding the context
 * from stored chunks, drain-to-replay once finished and all sources ended — is
 * preserved exactly; only the DOM/global side effects are replaced by callbacks.
 */
export class StreamingPlayback {
  private Ctor: AudioContextConstructor;
  private callbacks: StreamingPlaybackCallbacks;
  context: AudioContext | null = null;
  private nextStartTime = 0;
  private startedAt = 0;
  private seekOffset = 0;
  private pendingSources = 0;
  finished = false;
  stopped = false;
  private replayBlob: Blob | null = null;
  private replayLoaded = false;
  estimatedDuration = 0;
  private timer: ReturnType<typeof setInterval> | null = null;
  playing = true;
  private buffers: PcmChunk[] = [];
  private scheduledBufferCount = 0;
  private seekSerial = 0;
  private transportTransition: Promise<void> = Promise.resolve();

  constructor(options: StreamingPlaybackOptions = {}) {
    const resolve = options.audioContextCtor ?? audioContextCtor;
    const Ctor = resolve();
    if (!Ctor) throw new Error("Streaming playback is not supported by this browser.");
    this.Ctor = Ctor;
    this.callbacks = options.callbacks ?? {};
  }

  async start(): Promise<void> {
    this.context = new this.Ctor();
    this.startedAt = this.context.currentTime;
    this.nextStartTime = this.context.currentTime + 0.08;
    this.callbacks.onPlayingChange?.(true);
    await this.context.resume();
    this.timer = setInterval(() => this.updatePosition(), 250);
    this.updatePosition();
  }

  appendPcm(
    bytes: Uint8Array,
    sampleRate: number,
    channels = 1,
    waveformBytes: Uint8Array = bytes,
  ): void {
    if (this.stopped || !bytes?.length || !this.context) return;
    const duration = Math.floor(bytes.length / (2 * channels)) / sampleRate;
    this.buffers.push({ bytes, sampleRate, channels, duration });
    const { peaks, durationDelta } = streamingPcmPeaks(waveformBytes, sampleRate, channels);
    this.callbacks.onPeaks?.(peaks, durationDelta);
    this.estimatedDuration += duration;
    if (this.playing) this.schedulePendingBuffers();
    this.updatePosition();
  }

  private schedulePendingBuffers(): void {
    if (!this.context) return;
    while (this.scheduledBufferCount < this.buffers.length) {
      const first = this.buffers[this.scheduledBufferCount];
      let end = this.scheduledBufferCount + 1;
      while (
        end < this.buffers.length &&
        this.buffers[end].sampleRate === first.sampleRate &&
        this.buffers[end].channels === first.channels
      ) {
        end += 1;
      }
      const bytes =
        end === this.scheduledBufferCount + 1
          ? first.bytes
          : concatUint8Arrays(
              this.buffers.slice(this.scheduledBufferCount, end).map((chunk) => chunk.bytes),
            );
      const buffer = pcm16ToAudioBuffer(this.context, bytes, first.sampleRate, first.channels);
      this.scheduleBuffer(buffer, 0);
      this.scheduledBufferCount = end;
    }
  }

  private scheduleBuffer(buffer: AudioBuffer, offset = 0): void {
    if (!this.context) return;
    const sourceContext = this.context;
    const sourceSeekSerial = this.seekSerial;
    const source = this.context.createBufferSource();
    source.buffer = buffer;
    source.connect(this.context.destination);
    this.pendingSources += 1;
    source.onended = () => {
      if (this.stopped || this.context !== sourceContext || this.seekSerial !== sourceSeekSerial) {
        return;
      }
      this.pendingSources = Math.max(0, this.pendingSources - 1);
      this.checkDrain();
    };
    const startAt = Math.max(this.nextStartTime, this.context.currentTime + 0.03);
    source.start(startAt, offset);
    this.nextStartTime = startAt + Math.max(0, buffer.duration - offset);
  }

  async toggle(): Promise<void> {
    if (!this.context) return;
    this.playing = !this.playing;
    this.callbacks.onPlayingChange?.(this.playing);
    const context = this.context;
    const previous = this.transportTransition.catch(() => {});
    const transition = previous.then(async () => {
      if (this.stopped || this.context !== context) return;
      if (this.playing) {
        await context.resume();
        if (!this.stopped && this.context === context && this.playing) {
          this.schedulePendingBuffers();
        }
      } else {
        await context.suspend();
      }
    });
    this.transportTransition = transition;
    await transition;
    this.updatePosition();
  }

  async seekTo(seconds: number): Promise<void> {
    if (this.stopped || !this.context) return;
    const target = clamp(seconds, 0, this.estimatedDuration);
    const wasPlaying = this.playing;
    const seekSerial = this.seekSerial + 1;
    this.seekSerial = seekSerial;
    const previousContext = this.context;
    const nextContext = new this.Ctor();
    this.context = nextContext;
    this.pendingSources = 0;
    this.scheduledBufferCount = 0;
    this.nextStartTime = this.context.currentTime + 0.05;
    this.startedAt = this.context.currentTime;
    this.seekOffset = target;
    void previousContext?.close?.().catch(() => {});
    let cursor = 0;
    for (const chunk of this.buffers) {
      const chunkStart = cursor;
      const chunkEnd = chunkStart + chunk.duration;
      cursor = chunkEnd;
      if (chunkEnd <= target) continue;
      const offset = Math.max(0, target - chunkStart);
      const buffer = pcm16ToAudioBuffer(
        this.context,
        chunk.bytes,
        chunk.sampleRate,
        chunk.channels,
      );
      this.scheduleBuffer(buffer, offset);
    }
    this.scheduledBufferCount = this.buffers.length;
    try {
      if (wasPlaying) {
        await nextContext.resume();
      } else {
        await nextContext.suspend();
      }
    } catch (error) {
      if (this.seekSerial !== seekSerial || this.context !== nextContext || this.stopped) return;
      throw error;
    }
    if (this.seekSerial !== seekSerial || this.context !== nextContext || this.stopped) return;
    this.playing = wasPlaying;
    this.callbacks.onPlayingChange?.(wasPlaying);
    this.updatePosition();
  }

  setReplayBlob(blob: Blob): void {
    this.replayBlob = blob;
    this.checkDrain();
  }

  markFinished(): void {
    this.finished = true;
    this.callbacks.onFinished?.();
    this.checkDrain();
  }

  elapsedSeconds(): number {
    if (!this.context) return this.seekOffset;
    return clamp(
      this.seekOffset + Math.max(0, this.context.currentTime - this.startedAt),
      0,
      this.estimatedDuration,
    );
  }

  private updatePosition(): void {
    if (this.stopped) return;
    const current = this.elapsedSeconds();
    this.callbacks.onProgress?.(current, this.estimatedDuration, this.finished);
    if (
      this.finished &&
      this.replayBlob &&
      !this.replayLoaded &&
      this.context &&
      this.context.currentTime >= this.nextStartTime + 0.08
    ) {
      this.pendingSources = 0;
      this.checkDrain();
    }
  }

  private checkDrain(): void {
    if (this.stopped) return;
    if (
      !this.finished ||
      this.pendingSources > 0 ||
      this.scheduledBufferCount < this.buffers.length ||
      !this.replayBlob ||
      this.replayLoaded
    )
      return;
    this.replayLoaded = true;
    const blob = this.replayBlob;
    this.stop();
    this.callbacks.onReplayReady?.(blob);
  }

  stop(options: { keepButton?: boolean } = {}): void {
    this.stopped = true;
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    void this.context?.close?.().catch(() => {});
    if (!options.keepButton) this.callbacks.onPlayingChange?.(false);
  }
}

/** Callbacks for {@link createPcmStreamSink}. */
export interface PcmStreamSinkCallbacks {
  /** Coarse generation progress (mirrors legacy `setGenerateProgress`). */
  onProgress?: (fraction: number, label: string) => void;
  /** High-level streaming state transitions. */
  onStateChange?: (state: StreamState) => void;
}

/** Options for {@link createPcmStreamSink}. */
export interface PcmStreamSinkOptions {
  /** PCM gain multiplier; defaults to `2.0` (matches {@link applyPcm16Gain}'s default path). */
  gain?: number;
  /** Injectable `AudioContext` constructor resolver. */
  audioContextCtor?: () => AudioContextConstructor | null;
  /** Forwarded to the internal {@link StreamingPlayback}. */
  playbackCallbacks?: StreamingPlaybackCallbacks;
  callbacks?: PcmStreamSinkCallbacks;
}

/** The streaming sink returned by {@link createPcmStreamSink}. */
export interface PcmStreamSink {
  playback: StreamingPlayback;
  start(): Promise<void>;
  onAudioChunk(bytes: Uint8Array, meta?: { sampleRate?: number; channels?: number }): void;
  finish(): Blob;
  fail(): void;
}

/**
 * Build a PCM streaming sink over a {@link StreamingPlayback}.
 *
 * Ports `createPcmStreamSink` (app.html line ~2936): carries an odd trailing
 * byte across chunks ({@link evenPcmBytes}), applies gain ({@link applyPcm16Gain},
 * over the even bytes only), tracks sample rate/channels from chunk metadata,
 * feeds the playback the gained bytes (waveform from the pre-gain bytes), and
 * stitches a final WAV blob. `finish` throws if no audio arrived.
 */
export function createPcmStreamSink(options: PcmStreamSinkOptions = {}): PcmStreamSink {
  const playback = new StreamingPlayback({
    audioContextCtor: options.audioContextCtor,
    callbacks: options.playbackCallbacks,
  });
  const gain = options.gain ?? 2.0;
  const parts: Uint8Array[] = [];
  const pendingByte: PendingByte = { value: null };
  let sampleRate = 24000;
  let channels = 1;
  let started = false;
  return {
    playback,
    async start() {
      options.callbacks?.onStateChange?.("buffering");
      await playback.start();
    },
    onAudioChunk(bytes, meta = {}) {
      if (!bytes?.length) return;
      const pcm = evenPcmBytes(bytes, pendingByte);
      if (!pcm.length) return;
      const gained = applyPcm16Gain(pcm, gain);
      sampleRate = Number(meta.sampleRate) || sampleRate;
      channels = Number(meta.channels) || channels;
      parts.push(gained);
      if (!started) {
        started = true;
        options.callbacks?.onStateChange?.("playing");
      }
      playback.appendPcm(gained, sampleRate, channels, pcm);
      options.callbacks?.onProgress?.(0.72, "Streaming");
    },
    finish() {
      if (parts.length === 0) throw new Error("Streaming TTS did not return audio.");
      pendingByte.value = null;
      playback.markFinished();
      options.callbacks?.onStateChange?.("done");
      return wavBlobFromPcm(concatUint8Arrays(parts), sampleRate, channels);
    },
    fail() {
      playback.stop();
      options.callbacks?.onStateChange?.("error");
    },
  };
}

/** An abort-style error matching the legacy `generationAbortError`. */
export function streamAbortError(message = "Generation was cancelled."): Error {
  const error = new Error(message);
  error.name = "AbortError";
  return error;
}

function signalAbortError(signal: AbortSignal | null | undefined): Error {
  return signal?.reason instanceof Error ? signal.reason : streamAbortError();
}

/** Result of a streaming synthesis. */
export interface StreamResult {
  blob: Blob;
  playback: StreamingPlayback;
  streamingModel: string;
}

/** Options common to the streaming entry points. */
export interface StreamOptions {
  /** Settings `model` value for provider-model resolution. */
  settingsModel?: string | null;
  /** Abort signal forwarded to fetch/WebSocket. */
  signal?: AbortSignal | null;
  /** Cancellation check (throws to abort); mirrors `throwIfGenerationCancelled`. */
  throwIfCancelled?: () => void;
  /** Coarse progress callback (mirrors `setGenerateProgress`). */
  onProgress?: (fraction: number, label: string) => void;
  /** High-level state callback forwarded to the sink. */
  onStateChange?: (state: StreamState) => void;
  /** Injectable `AudioContext` constructor resolver. */
  audioContextCtor?: () => AudioContextConstructor | null;
  /** Forwarded to the internal {@link StreamingPlayback}. */
  playbackCallbacks?: StreamingPlaybackCallbacks;
  /** The live playback exists and can be controlled while bytes continue arriving. */
  onPlaybackReady?: (playback: StreamingPlayback) => void;
}

function ensureNotCancelled(options: StreamOptions): void {
  if (options.signal?.aborted) throw signalAbortError(options.signal);
  options.throwIfCancelled?.();
}

function sinkOptionsFrom(config: BrowserTtsConfig, options: StreamOptions): PcmStreamSinkOptions {
  return {
    gain: ttsStreamPcmGain(config),
    audioContextCtor: options.audioContextCtor,
    playbackCallbacks: options.playbackCallbacks,
    callbacks: { onProgress: options.onProgress, onStateChange: options.onStateChange },
  };
}

/** Normalize the Google interactions base URL. Ports `googleInteractionsBaseUrl`. */
export function googleInteractionsBaseUrl(baseUrl: string | null | undefined): string {
  return String(baseUrl || "https://generativelanguage.googleapis.com/v1beta")
    .replace(/\/$/, "")
    .replace(/\/models$/, "");
}

interface GoogleInteractionEvent {
  event_type?: string;
  eventType?: string;
  delta?: { type?: string; data?: string; audio?: string };
  step?: { delta?: { type?: string; data?: string; audio?: string } };
  output_audio?: { type?: string; data?: string; audio?: string };
  outputAudio?: { type?: string; data?: string; audio?: string };
}

/**
 * Read a Google interactions SSE stream into the sink. Ports
 * `readGoogleInteractionStream` (app.html line ~3163).
 */
export async function readGoogleInteractionStream(
  response: Response,
  sink: PcmStreamSink,
  sampleRate: number,
  channels: number,
  options: StreamOptions = {},
): Promise<void> {
  if (!response.body?.getReader) throw new Error("Google streaming response is not readable.");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  const handleEvent = (raw: string): void => {
    const data = raw
      .split(/\r?\n/)
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).trim())
      .join("");
    const payload = data || raw.trim();
    if (!payload || payload === "[DONE]") return;
    const event = JSON.parse(payload) as GoogleInteractionEvent;
    const delta = event.delta || event.step?.delta || event.output_audio || event.outputAudio;
    if (
      event.event_type === "step.delta" ||
      event.eventType === "step.delta" ||
      delta?.type === "audio"
    ) {
      const audioData = delta?.data || delta?.audio || event.delta?.data;
      if (audioData) sink.onAudioChunk(bytesFromBase64(audioData), { sampleRate, channels });
    }
  };
  for (;;) {
    ensureNotCancelled(options);
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let boundary: number;
    while ((boundary = buffer.search(/\r?\n\r?\n/)) >= 0) {
      const raw = buffer.slice(0, boundary);
      buffer = buffer.slice(buffer[boundary] === "\r" ? boundary + 4 : boundary + 2);
      handleEvent(raw);
    }
  }
  buffer += decoder.decode();
  if (buffer.trim()) handleEvent(buffer);
}

/**
 * Stream Google TTS via the `/interactions` endpoint. Ports `streamGoogle`
 * (app.html line ~3199).
 */
export async function streamGoogle(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  instructions: string | null | undefined,
  options: StreamOptions = {},
): Promise<StreamResult> {
  ensureNotCancelled(options);
  const google = config.providers?.google;
  if (!google || !canStreamGoogle(config, options.settingsModel)) {
    throw new Error("Google streaming is not available for this model.");
  }
  const streamConfig = google.streaming || ({} as NonNullable<typeof google.streaming>);
  const sampleRate = Number(streamConfig.sampleRate) || 24000;
  const channels = Number(streamConfig.channels) || 1;
  const model = normalizeGoogleModelName(resolveGoogleModel(google, options.settingsModel));
  const voiceName = persona?.google?.voiceName || google.voice;
  const sink = createPcmStreamSink(sinkOptionsFrom(config, options));
  options.onPlaybackReady?.(sink.playback);
  await sink.start();
  options.onProgress?.(0.48, "Connecting");
  const response = await fetch(`${googleInteractionsBaseUrl(google.baseUrl)}/interactions`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Api-Revision": "2026-05-20",
      "x-goog-api-key": google.apiKey,
    },
    body: JSON.stringify({
      model,
      input: buildGoogleTtsPrompt(input, persona, instructions),
      response_format: { type: "audio" },
      generation_config: { speech_config: [{ voice: voiceName }] },
      stream: true,
    }),
    signal: options.signal ?? null,
  });
  if (!response.ok) {
    sink.fail();
    throw await providerError(response, "Google streaming TTS failed");
  }
  try {
    await readGoogleInteractionStream(response, sink, sampleRate, channels, options);
    const blob = sink.finish();
    return { blob, playback: sink.playback, streamingModel: model };
  } catch (error) {
    sink.fail();
    throw error;
  }
}

interface ElevenLabsStreamMeta {
  outputFormat: string;
  sampleRate: number;
  channels: number;
  modelId: string;
}

/**
 * Stream ElevenLabs TTS over the HTTP streaming endpoint. Ports
 * `streamElevenLabsHttp` (app.html line ~3103).
 */
export async function streamElevenLabsHttp(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  streamMeta: ElevenLabsStreamMeta,
  options: StreamOptions = {},
): Promise<StreamResult> {
  ensureNotCancelled(options);
  const elevenlabs = config.providers?.elevenlabs;
  if (!elevenlabs) throw new Error("ElevenLabs TTS is not configured.");
  const voiceId = persona?.elevenlabs?.voiceId;
  if (!voiceId) throw new Error("ElevenLabs voice_id is not configured for this persona.");
  const outputFormat = streamMeta.outputFormat || "pcm_24000";
  const sampleRate = Number(streamMeta.sampleRate) || elevenLabsSampleRate(outputFormat);
  const channels = Number(streamMeta.channels) || 1;
  const modelId =
    streamMeta.modelId || resolveElevenLabsStreamingModel(elevenlabs, options.settingsModel);
  const sink = createPcmStreamSink(sinkOptionsFrom(config, options));
  options.onPlaybackReady?.(sink.playback);
  await sink.start();
  options.onProgress?.(0.48, "Connecting");
  const url = new URL(
    `${String(elevenlabs.baseUrl || "https://api.elevenlabs.io").replace(
      /\/$/,
      "",
    )}/v1/text-to-speech/${encodeURIComponent(voiceId)}/stream`,
  );
  url.searchParams.set("output_format", outputFormat);
  const voiceSettings = persona?.elevenlabs?.voiceSettings
    ? { ...persona.elevenlabs.voiceSettings, speed: resolveElevenLabsSpeed(persona) }
    : { speed: 1.0 };
  const body: Record<string, unknown> = {
    text: input,
    model_id: modelId,
    voice_settings: voiceSettings,
    apply_text_normalization: elevenlabs.applyTextNormalization,
  };
  if (elevenlabs.languageCode) body.language_code = elevenlabs.languageCode;
  const response = await fetch(url.toString(), {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "xi-api-key": elevenlabs.apiKey,
    },
    body: JSON.stringify(body),
    signal: options.signal ?? null,
  });
  if (!response.ok) {
    sink.fail();
    throw await providerError(response, "ElevenLabs streaming TTS failed");
  }
  if (!response.body?.getReader) {
    sink.fail();
    throw new Error("ElevenLabs streaming response is not readable.");
  }
  try {
    const reader = response.body.getReader();
    for (;;) {
      ensureNotCancelled(options);
      const { value, done } = await reader.read();
      if (done) break;
      if (value) sink.onAudioChunk(value, { sampleRate, channels });
    }
    const blob = sink.finish();
    return { blob, playback: sink.playback, streamingModel: modelId };
  } catch (error) {
    sink.fail();
    throw error;
  }
}

/**
 * Stream ElevenLabs TTS, choosing the WebSocket or HTTP transport. Ports
 * `streamElevenLabs` (app.html line ~3006). WebSocket is used when the model
 * supports it and `WebSocket` exists; otherwise the HTTP stream is used.
 */
export async function streamElevenLabs(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  options: StreamOptions = {},
): Promise<StreamResult> {
  ensureNotCancelled(options);
  const elevenlabs = config.providers?.elevenlabs;
  if (!elevenlabs || !canStreamElevenLabs(config, persona, options.settingsModel)) {
    throw new Error("ElevenLabs streaming is not available.");
  }
  const streamConfig = elevenlabs.streaming || ({} as NonNullable<typeof elevenlabs.streaming>);
  const outputFormat = streamConfig.outputFormat || "pcm_24000";
  const sampleRate = Number(streamConfig.sampleRate) || elevenLabsSampleRate(outputFormat);
  const channels = Number(streamConfig.channels) || 1;
  const modelId = resolveElevenLabsStreamingModel(elevenlabs, options.settingsModel);
  if (!elevenLabsWebSocketModelSupported(modelId)) {
    return streamElevenLabsHttp(
      config,
      input,
      persona,
      { outputFormat, sampleRate, channels, modelId },
      options,
    );
  }
  const voiceId = persona?.elevenlabs?.voiceId;
  if (!voiceId) throw new Error("ElevenLabs voice_id is not configured for this persona.");
  const url = new URL(
    `${websocketBaseUrl(elevenlabs.baseUrl)}/v1/text-to-speech/${encodeURIComponent(
      voiceId,
    )}/stream-input`,
  );
  url.searchParams.set("model_id", modelId);
  url.searchParams.set("output_format", outputFormat);
  if (elevenlabs.languageCode) url.searchParams.set("language_code", elevenlabs.languageCode);
  if (elevenlabs.applyTextNormalization) {
    url.searchParams.set("apply_text_normalization", elevenlabs.applyTextNormalization);
  }

  const sink = createPcmStreamSink(sinkOptionsFrom(config, options));
  options.onPlaybackReady?.(sink.playback);
  await sink.start();
  if (options.signal?.aborted) {
    sink.fail();
    throw signalAbortError(options.signal);
  }
  options.onProgress?.(0.48, "Connecting");

  return new Promise<StreamResult>((resolve, reject) => {
    let settled = false;
    let opened = false;
    let receivedAudio = false;
    const socket = new WebSocket(url.toString());
    const signal = options.signal ?? null;
    function fail(error: unknown): void {
      if (settled) return;
      signal?.removeEventListener("abort", abort);
      settled = true;
      sink.fail();
      reject(
        error instanceof Error ? error : new Error(String(error || "ElevenLabs stream failed.")),
      );
    }
    function abort(): void {
      try {
        socket.close();
      } catch {
        // Ignored.
      }
      fail(signalAbortError(signal));
    }
    if (signal?.aborted) {
      abort();
      return;
    }
    signal?.addEventListener("abort", abort, { once: true });
    const finish = (): void => {
      if (settled) return;
      signal?.removeEventListener("abort", abort);
      try {
        const blob = sink.finish();
        settled = true;
        resolve({ blob, playback: sink.playback, streamingModel: modelId });
      } catch (error) {
        fail(error);
      }
    };
    socket.addEventListener("open", () => {
      opened = true;
      const voiceSettings = persona?.elevenlabs?.voiceSettings
        ? { ...persona.elevenlabs.voiceSettings, speed: resolveElevenLabsSpeed(persona) }
        : { speed: 1.0 };
      socket.send(
        JSON.stringify({
          text: " ",
          voice_settings: voiceSettings,
          generation_config: {
            chunk_length_schedule: streamConfig.chunkLengthSchedule || [120, 160, 250, 290],
          },
          xi_api_key: elevenlabs.apiKey,
        }),
      );
      socket.send(JSON.stringify({ text: input, flush: true }));
      socket.send(JSON.stringify({ text: "" }));
    });
    socket.addEventListener("message", (event: MessageEvent) => {
      try {
        const data = JSON.parse(String(event.data)) as { audio?: string; isFinal?: boolean };
        if (data.audio) {
          receivedAudio = true;
          sink.onAudioChunk(bytesFromBase64(data.audio), { sampleRate, channels });
        }
        if (data.isFinal) {
          socket.close();
          finish();
        }
      } catch (error) {
        try {
          socket.close();
        } catch {
          // Ignored.
        }
        fail(error);
      }
    });
    socket.addEventListener("error", () => fail(new Error("ElevenLabs WebSocket stream failed.")));
    socket.addEventListener("close", () => {
      if (!settled && opened && receivedAudio) finish();
      if (!settled) fail(new Error("ElevenLabs WebSocket closed before streaming audio."));
    });
  });
}

/**
 * Try to stream a provider, returning `null` when streaming is unavailable.
 * Ports `tryStreamProvider` (app.html line ~3243).
 */
export async function tryStreamProvider(
  config: BrowserTtsConfig,
  provider: string,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  instructions: string | null | undefined,
  options: StreamOptions = {},
): Promise<StreamResult | null> {
  if (provider === "elevenlabs" && canStreamElevenLabs(config, persona, options.settingsModel)) {
    const timed = providerTimeoutSignal(
      config.providers.elevenlabs!.timeoutMs,
      input,
      options.signal,
    );
    try {
      return await streamElevenLabs(config, input, persona, { ...options, signal: timed.signal });
    } finally {
      timed.dispose();
    }
  }
  if (provider === "google" && canStreamGoogle(config, options.settingsModel)) {
    const timed = providerTimeoutSignal(config.providers.google!.timeoutMs, input, options.signal);
    try {
      return await streamGoogle(config, input, persona, instructions, {
        ...options,
        signal: timed.signal,
      });
    } finally {
      timed.dispose();
    }
  }
  return null;
}
