import type {
  BrowserElevenLabsConfig,
  BrowserGoogleConfig,
  BrowserPersonaConfig,
  BrowserTtsConfig,
} from "../config.ts";
import { decodeAudioPeaks, streamingPcmPeaks } from "./waveform.ts";
import { bytesFromBase64, concatUint8Arrays, wavBlobFromPcm } from "./wav.ts";

export interface DirectStreamCallbacks {
  onReady?: (playback: DirectStreamPlayback) => void;
  onPlayingChange?: (playing: boolean) => void;
  onProgress?: (currentSeconds: number, bufferedSeconds: number, finished: boolean) => void;
  onWaveformChunk?: (peaks: number[], durationDelta: number, sampleRate: number) => void;
  onWaveformSnapshot?: (peaks: number[], duration: number) => void;
  onFinished?: () => void;
  onDrained?: () => void;
}

export interface DirectStreamPlayback {
  readonly playing: boolean;
  readonly stopped: boolean;
  readonly seekable: boolean;
  toggle(): Promise<void>;
  seekTo(seconds: number): Promise<void>;
  stop(): void;
}

class MediaStreamPlayback implements DirectStreamPlayback {
  private readonly audio = new Audio();
  private readonly mediaSource = new MediaSource();
  private readonly objectUrl = URL.createObjectURL(this.mediaSource);
  private readonly audioContext: AudioContext | null;
  private sourceBuffer: SourceBuffer | null = null;
  private queue: Uint8Array[] = [];
  private streamFinished = false;
  private autoplayAttempted = false;
  private callbacks: DirectStreamCallbacks;
  playing = false;
  stopped = false;
  readonly seekable = true;

  constructor(callbacks: DirectStreamCallbacks, gain: number) {
    this.callbacks = callbacks;
    const Ctor = audioContextConstructor();
    this.audioContext = Ctor ? new Ctor() : null;
    if (this.audioContext) {
      const source = this.audioContext.createMediaElementSource(this.audio);
      const gainNode = this.audioContext.createGain();
      gainNode.gain.value = Math.max(0, gain);
      source.connect(gainNode).connect(this.audioContext.destination);
    } else {
      this.audio.volume = Math.min(1, Math.max(0, gain));
    }
    this.audio.src = this.objectUrl;
    this.audio.preload = "auto";
    this.audio.addEventListener("play", this.onPlay);
    this.audio.addEventListener("pause", this.onPause);
    this.audio.addEventListener("timeupdate", this.onTimeUpdate);
    this.audio.addEventListener("ended", this.onEnded);
  }

  private onPlay = (): void => {
    this.playing = true;
    this.callbacks.onPlayingChange?.(true);
  };

  private onPause = (): void => {
    this.playing = false;
    this.callbacks.onPlayingChange?.(false);
  };

  private onTimeUpdate = (): void => {
    const buffered =
      this.audio.buffered.length > 0 ? this.audio.buffered.end(this.audio.buffered.length - 1) : 0;
    this.callbacks.onProgress?.(this.audio.currentTime, buffered, this.streamFinished);
  };

  private onEnded = (): void => {
    this.callbacks.onDrained?.();
  };

  async start(signal: AbortSignal): Promise<void> {
    if (!MediaSource.isTypeSupported("audio/mpeg")) {
      throw new Error("MP3 streaming is not supported by this browser.");
    }
    await new Promise<void>((resolve, reject) => {
      const onOpen = (): void => {
        cleanup();
        try {
          this.sourceBuffer = this.mediaSource.addSourceBuffer("audio/mpeg");
          this.sourceBuffer.mode = "sequence";
          this.sourceBuffer.addEventListener("updateend", this.flush);
          resolve();
        } catch (error) {
          reject(error);
        }
      };
      const onError = (): void => {
        cleanup();
        reject(new Error("The browser could not start MP3 streaming."));
      };
      const onAbort = (): void => {
        cleanup();
        reject(
          signal.reason instanceof Error
            ? signal.reason
            : new DOMException("Aborted", "AbortError"),
        );
      };
      const cleanup = (): void => {
        this.mediaSource.removeEventListener("sourceopen", onOpen);
        this.mediaSource.removeEventListener("sourceclose", onError);
        signal.removeEventListener("abort", onAbort);
      };
      if (signal.aborted) {
        onAbort();
        return;
      }
      this.mediaSource.addEventListener("sourceopen", onOpen, { once: true });
      this.mediaSource.addEventListener("sourceclose", onError, { once: true });
      signal.addEventListener("abort", onAbort, { once: true });
    });
    await this.audioContext?.resume();
  }

  append(bytes: Uint8Array): void {
    if (this.stopped || !bytes.byteLength) return;
    this.queue.push(bytes.slice());
    this.flush();
  }

  private flush = (): void => {
    const source = this.sourceBuffer;
    if (this.stopped || !source || source.updating) return;
    const next = this.queue.shift();
    if (next) {
      source.appendBuffer(next as BufferSource);
      if (!this.autoplayAttempted) {
        this.autoplayAttempted = true;
        void this.audio.play().catch(() => {
          // The visible play control remains enabled if autoplay is denied.
        });
      }
      return;
    }
    if (this.streamFinished && this.mediaSource.readyState === "open") {
      this.mediaSource.endOfStream();
      this.callbacks.onFinished?.();
    }
  };

  markFinished(): void {
    this.streamFinished = true;
    this.flush();
  }

  async toggle(): Promise<void> {
    if (this.stopped) return;
    if (this.audio.paused) {
      await this.audioContext?.resume();
      await this.audio.play();
    } else this.audio.pause();
  }

  async seekTo(seconds: number): Promise<void> {
    if (this.stopped) return;
    const duration = Number.isFinite(this.audio.duration) ? this.audio.duration : seconds;
    this.audio.currentTime = Math.max(0, Math.min(duration, seconds));
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.audio.pause();
    this.sourceBuffer?.removeEventListener("updateend", this.flush);
    this.audio.removeEventListener("play", this.onPlay);
    this.audio.removeEventListener("pause", this.onPause);
    this.audio.removeEventListener("timeupdate", this.onTimeUpdate);
    this.audio.removeEventListener("ended", this.onEnded);
    this.audio.removeAttribute("src");
    this.audio.load();
    void this.audioContext?.close().catch(() => {});
    URL.revokeObjectURL(this.objectUrl);
  }
}

type AudioContextConstructor = new () => AudioContext;

function audioContextConstructor(): AudioContextConstructor | null {
  return (
    window.AudioContext ??
    (window as typeof window & { webkitAudioContext?: AudioContextConstructor })
      .webkitAudioContext ??
    null
  );
}

class PcmStreamPlayback implements DirectStreamPlayback {
  private readonly context: AudioContext;
  private nextStart = 0;
  private startedAt = 0;
  private bufferedSeconds = 0;
  private pendingSources = 0;
  private finished = false;
  private timer: number | null = null;
  playing = false;
  stopped = false;
  readonly seekable = false;

  constructor(
    Ctor: AudioContextConstructor,
    private readonly callbacks: DirectStreamCallbacks,
  ) {
    this.context = new Ctor();
    this.nextStart = this.context.currentTime + 0.08;
    this.startedAt = this.context.currentTime;
  }

  async start(): Promise<void> {
    await this.context.resume();
    this.playing = true;
    this.callbacks.onPlayingChange?.(true);
    this.timer = window.setInterval(() => this.update(), 200);
  }

  append(bytes: Uint8Array, sampleRate = 24_000): void {
    if (this.stopped || bytes.byteLength < 2) return;
    const samples = Math.floor(bytes.byteLength / 2);
    const buffer = this.context.createBuffer(1, samples, sampleRate);
    const channel = buffer.getChannelData(0);
    const view = new DataView(bytes.buffer, bytes.byteOffset, samples * 2);
    for (let index = 0; index < samples; index += 1) {
      channel[index] = view.getInt16(index * 2, true) / 32768;
    }
    const source = this.context.createBufferSource();
    source.buffer = buffer;
    source.connect(this.context.destination);
    this.pendingSources += 1;
    source.onended = () => {
      if (this.stopped) return;
      this.pendingSources = Math.max(0, this.pendingSources - 1);
      this.checkDrained();
    };
    const startAt = Math.max(this.nextStart, this.context.currentTime + 0.03);
    source.start(startAt);
    this.nextStart = startAt + buffer.duration;
    this.bufferedSeconds += buffer.duration;
    const waveform = streamingPcmPeaks(bytes, sampleRate, 1);
    this.callbacks.onWaveformChunk?.(waveform.peaks, waveform.durationDelta, sampleRate);
    this.update();
  }

  markFinished(): void {
    this.finished = true;
    this.callbacks.onFinished?.();
    this.checkDrained();
  }

  private update(): void {
    if (this.stopped) return;
    const elapsed = Math.min(
      this.bufferedSeconds,
      Math.max(0, this.context.currentTime - this.startedAt),
    );
    this.callbacks.onProgress?.(elapsed, this.bufferedSeconds, this.finished);
  }

  private checkDrained(): void {
    if (this.finished && this.pendingSources === 0) this.callbacks.onDrained?.();
  }

  async toggle(): Promise<void> {
    if (this.stopped) return;
    this.playing = !this.playing;
    if (this.playing) await this.context.resume();
    else await this.context.suspend();
    this.callbacks.onPlayingChange?.(this.playing);
  }

  async seekTo(): Promise<void> {
    // Live PCM is not seekable; replay becomes seekable after the stream drains.
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    if (this.timer !== null) window.clearInterval(this.timer);
    this.timer = null;
    void this.context.close().catch(() => {});
    this.callbacks.onPlayingChange?.(false);
  }
}

function resolveModel(config: BrowserElevenLabsConfig, setting: string): string {
  if (setting.startsWith("elevenlabs:")) return setting.slice("elevenlabs:".length);
  return config.models[0] || "eleven_v3";
}

export function canStreamElevenLabsDirect(config: BrowserTtsConfig, modelSetting: string): boolean {
  const provider = config.providers.elevenlabs;
  if (!provider) return false;
  const model = resolveModel(provider, modelSetting).toLowerCase();
  return model === "eleven_v3" || model.startsWith("eleven_v3_");
}

function resolveGoogleModel(config: BrowserGoogleConfig, setting: string): string {
  if (setting.startsWith("google:")) return setting.slice("google:".length);
  return config.models[0] || "";
}

export function canStreamGoogleDirect(config: BrowserTtsConfig, modelSetting: string): boolean {
  const provider = config.providers.google;
  return Boolean(provider && /^gemini-3\.1-/i.test(resolveGoogleModel(provider, modelSetting)));
}

function streamError(status: number, body: string): Error {
  let detail = body.trim();
  try {
    const parsed = JSON.parse(body) as { detail?: { message?: string } | string };
    detail = typeof parsed.detail === "string" ? parsed.detail : parsed.detail?.message || detail;
  } catch {
    // Preserve a plain-text provider response.
  }
  return new Error(detail || `ElevenLabs streaming failed (${status}).`);
}

export function directStreamTimeoutMs(baseMs: number, input: string): number {
  const normalizedBase = Math.max(250, baseMs || 30_000);
  const chars = Array.from(input).length;
  if (chars <= 1_200) return normalizedBase;
  const scaledMs = Math.min(300_000, Math.max(90_000, Math.floor(chars / 25) * 1_000));
  return Math.min(300_000, Math.max(normalizedBase, scaledMs));
}

export async function streamElevenLabsDirect(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig,
  modelSetting: string,
  signal: AbortSignal,
  callbacks: DirectStreamCallbacks,
): Promise<{ blob: Blob; playback: DirectStreamPlayback }> {
  const provider = config.providers.elevenlabs;
  const voice = persona.elevenlabs;
  if (!provider || !voice) throw new Error("ElevenLabs streaming is unavailable.");

  const playback = new MediaStreamPlayback(callbacks, provider.streamGain);
  const requestController = new AbortController();
  const forwardAbort = (): void => requestController.abort(signal.reason);
  signal.addEventListener("abort", forwardAbort, { once: true });
  const timeout = window.setTimeout(
    () =>
      requestController.abort(new DOMException("ElevenLabs streaming timed out.", "TimeoutError")),
    directStreamTimeoutMs(provider.timeoutMs, input),
  );
  const url = new URL(
    `${provider.baseUrl.replace(/\/$/, "")}/v1/text-to-speech/${encodeURIComponent(voice.voiceId)}/stream`,
  );
  url.searchParams.set("output_format", "mp3_44100_128");
  const body: Record<string, unknown> = {
    text: input,
    model_id: resolveModel(provider, modelSetting),
    voice_settings: voice.voiceSettings,
    apply_text_normalization: provider.applyTextNormalization,
  };
  if (provider.languageCode) body.language_code = provider.languageCode;

  const parts: Uint8Array[] = [];
  let receivedBytes = 0;
  let previewChain = Promise.resolve();
  let previewBytes = 0;
  let previewAt = 0;
  const updatePreview = (force = false): Promise<void> => {
    const now = performance.now();
    if (!force && (receivedBytes - previewBytes < 16 * 1024 || now - previewAt < 400)) {
      return previewChain;
    }
    previewBytes = receivedBytes;
    previewAt = now;
    const snapshot = new Blob(parts as BlobPart[], { type: "audio/mpeg" });
    previewChain = previewChain.then(async () => {
      try {
        const decoded = await decodeAudioPeaks(snapshot);
        if (!requestController.signal.aborted && decoded) {
          callbacks.onWaveformSnapshot?.(decoded.peaks, decoded.duration);
        }
      } catch {
        // An early partial MP3 frame may not be decodable yet. A later chunk
        // retries with the full byte prefix accumulated so far.
      }
    });
    return previewChain;
  };
  try {
    await playback.start(requestController.signal);
    if (requestController.signal.aborted) throw requestController.signal.reason;
    callbacks.onReady?.(playback);
    const response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json", "xi-api-key": provider.apiKey },
      body: JSON.stringify(body),
      signal: requestController.signal,
    });
    if (!response.ok) throw streamError(response.status, await response.text());
    if (!response.body) throw new Error("ElevenLabs streaming response is not readable.");
    const reader = response.body.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (!value?.byteLength) continue;
      const chunk = value.slice();
      parts.push(chunk);
      receivedBytes += chunk.byteLength;
      playback.append(chunk);
      void updatePreview();
    }
    if (!parts.length) throw new Error("ElevenLabs streaming returned no audio.");
    await updatePreview(true);
    if (requestController.signal.aborted) throw requestController.signal.reason;
    playback.markFinished();
    return { blob: new Blob(parts as BlobPart[], { type: "audio/mpeg" }), playback };
  } catch (error) {
    playback.stop();
    throw error;
  } finally {
    window.clearTimeout(timeout);
    signal.removeEventListener("abort", forwardAbort);
  }
}

interface GoogleInteractionEvent {
  event_type?: string;
  eventType?: string;
  delta?: { type?: string; data?: string; audio?: string };
  step?: { delta?: { type?: string; data?: string; audio?: string } };
  output_audio?: { type?: string; data?: string; audio?: string };
  outputAudio?: { type?: string; data?: string; audio?: string };
}

export async function streamGoogleDirect(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null,
  modelSetting: string,
  signal: AbortSignal,
  callbacks: DirectStreamCallbacks,
): Promise<{ blob: Blob; playback: DirectStreamPlayback }> {
  const provider = config.providers.google;
  if (!provider || !canStreamGoogleDirect(config, modelSetting)) {
    throw new Error("Google direct streaming is unavailable for this model.");
  }

  const Ctor = audioContextConstructor();
  if (!Ctor) throw new Error("Google PCM streaming is unsupported by this browser.");
  const playback = new PcmStreamPlayback(Ctor, callbacks);
  const requestController = new AbortController();
  const forwardAbort = (): void => requestController.abort(signal.reason);
  signal.addEventListener("abort", forwardAbort, { once: true });
  const timeout = window.setTimeout(
    () => requestController.abort(new DOMException("Google streaming timed out.", "TimeoutError")),
    directStreamTimeoutMs(provider.timeoutMs, input),
  );
  const model = resolveGoogleModel(provider, modelSetting);
  const voice = persona?.google?.voiceName || provider.voice;
  const parts: Uint8Array[] = [];
  try {
    await playback.start();
    if (requestController.signal.aborted) throw requestController.signal.reason;
    callbacks.onReady?.(playback);
    const response = await fetch("/web/google-stream", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        input,
        model,
        voice,
      }),
      signal: requestController.signal,
    });
    if (!response.ok) throw streamError(response.status, await response.text());
    if (!response.body) throw new Error("Google streaming response is not readable.");

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let pending = "";
    const consumeEvent = (raw: string): void => {
      const payload = raw
        .split(/\r?\n/)
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trim())
        .join("");
      if (!payload || payload === "[DONE]") return;
      const event = JSON.parse(payload) as GoogleInteractionEvent;
      const delta = event.delta || event.step?.delta || event.output_audio || event.outputAudio;
      const audio = delta?.data || delta?.audio;
      if (
        audio &&
        (event.event_type === "step.delta" ||
          event.eventType === "step.delta" ||
          delta?.type === "audio")
      ) {
        const bytes = bytesFromBase64(audio);
        parts.push(bytes);
        playback.append(bytes);
      }
    };
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      pending += decoder.decode(value, { stream: true });
      let boundary = pending.search(/\r?\n\r?\n/);
      while (boundary >= 0) {
        const raw = pending.slice(0, boundary);
        const separatorLength = pending[boundary] === "\r" ? 4 : 2;
        pending = pending.slice(boundary + separatorLength);
        consumeEvent(raw);
        boundary = pending.search(/\r?\n\r?\n/);
      }
    }
    pending += decoder.decode();
    if (pending.trim()) consumeEvent(pending);
    if (!parts.length) throw new Error("Google streaming returned no audio.");
    playback.markFinished();
    return { blob: wavBlobFromPcm(concatUint8Arrays(parts), 24_000), playback };
  } catch (error) {
    playback.stop();
    throw error;
  } finally {
    window.clearTimeout(timeout);
    signal.removeEventListener("abort", forwardAbort);
  }
}
