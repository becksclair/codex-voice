/**
 * Browser-direct ElevenLabs TTS client.
 *
 * Ports the non-streaming ElevenLabs path plus streaming-support predicates
 * from app.html:
 * - `resolveElevenLabsModel` (line ~1897), `resolveElevenLabsStreamingModel`
 *   (line ~2982)
 * - `websocketBaseUrl` (line ~2971), `elevenLabsWebSocketModelSupported`
 *   (line ~2977), `canStreamElevenLabs` (line ~2986)
 * - `resolveElevenLabsSpeed` (line ~3332), `elevenLabsMimeType` (line ~3337),
 *   `elevenLabsSampleRate` (line ~3344)
 * - `synthesizeElevenLabsSingle` (line ~3349), `synthesizeElevenLabs`
 *   (line ~3386)
 *
 * The WebSocket/HTTP streaming playback path (`streamElevenLabs`,
 * `streamElevenLabsHttp`) is UI/AudioContext-playback orchestration and is
 * intentionally left to B2. Request shapes here are faithful to the live API.
 */

import { audioContextCtor } from "../audio/waveform.ts";
import { concatPcmChunksWithBoundarySilence, wavBlobFromPcm } from "../audio/wav.ts";
import type { BrowserElevenLabsConfig, BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import { clamp } from "../util.ts";
import { TTS_CHUNK_MIN_CHARS, splitTtsText } from "./chunking.ts";
import { providerError, selectedProviderModel } from "./common.ts";
import { synthesizeChunksOrdered } from "./pool.ts";
import { providerTimeoutSignal } from "./timeout.ts";

/** Options for a single ElevenLabs synth request. */
export interface ElevenLabsSingleOptions {
  /** Overrides `elevenlabs.outputFormat` (e.g. `'pcm_24000'` for chunk stitching). */
  outputFormat?: string | null;
  /** When true and the format is PCM, return the raw bytes instead of a WAV blob. */
  rawPcm?: boolean;
  /** Model id to request. Defaults to the config's `elevenlabs.modelId`. */
  model?: string;
  /** Abort signal forwarded to `fetch`. */
  signal?: AbortSignal | null;
  /** Cancellation check invoked before the request (throws to abort). */
  throwIfCancelled?: () => void;
}

/** Options for the chunked ElevenLabs entry point. */
export interface ElevenLabsSynthOptions {
  /** Model id to request. Defaults to the config's `elevenlabs.modelId`. */
  model?: string;
  signal?: AbortSignal | null;
  throwIfCancelled?: () => void;
}

/**
 * Resolve the ElevenLabs model id from the settings `model` value.
 * Ports `resolveElevenLabsModel` (app.html line ~1897).
 */
export function resolveElevenLabsModel(
  elevenlabs: BrowserElevenLabsConfig | null | undefined,
  settingsModel?: string | null,
): string {
  if (!elevenlabs) return "";
  return selectedProviderModel(settingsModel, "elevenlabs", elevenlabs.modelId);
}

/**
 * Resolve the model used for streaming. Ports `resolveElevenLabsStreamingModel`
 * (app.html line ~2982), which is currently identical to
 * {@link resolveElevenLabsModel}.
 */
export function resolveElevenLabsStreamingModel(
  elevenlabs: BrowserElevenLabsConfig | null | undefined,
  settingsModel?: string | null,
): string {
  return resolveElevenLabsModel(elevenlabs, settingsModel);
}

/**
 * Convert an HTTP base URL to a WebSocket base URL.
 *
 * Ports `websocketBaseUrl` (app.html line ~2971): swaps `http:`→`ws:` and any
 * other protocol→`wss:`, defaults to `https://api.elevenlabs.io`, and strips a
 * trailing slash.
 */
export function websocketBaseUrl(baseUrl: string | null | undefined): string {
  const url = new URL(baseUrl || "https://api.elevenlabs.io");
  url.protocol = url.protocol === "http:" ? "ws:" : "wss:";
  return url.toString().replace(/\/$/, "");
}

/**
 * Whether a model supports the WebSocket streaming transport.
 *
 * Ports `elevenLabsWebSocketModelSupported` (app.html line ~2977): any non-empty
 * model that does not start with `eleven_v3`.
 */
export function elevenLabsWebSocketModelSupported(model: string | null | undefined): boolean {
  const normalized = String(model || "").toLowerCase();
  return Boolean(normalized) && !normalized.startsWith("eleven_v3");
}

/**
 * Whether ElevenLabs streaming is usable for the current config/persona.
 *
 * Ports `canStreamElevenLabs` (app.html line ~2986): requires the provider
 * config, an `AudioContext`, an API key, and a persona `voiceId`. Models that
 * support the WebSocket transport require `window.WebSocket`; others fall back
 * to the HTTP stream and require `window.ReadableStream`.
 */
export function canStreamElevenLabs(
  config: BrowserTtsConfig | null | undefined,
  persona: BrowserPersonaConfig | null | undefined,
  settingsModel?: string | null,
): boolean {
  const elevenlabs = config?.providers?.elevenlabs;
  if (!elevenlabs || !audioContextCtor() || !elevenlabs.apiKey || !persona?.elevenlabs?.voiceId) {
    return false;
  }
  const model = resolveElevenLabsStreamingModel(elevenlabs, settingsModel);
  return elevenLabsWebSocketModelSupported(model)
    ? Boolean(window.WebSocket)
    : Boolean(window.ReadableStream);
}

/**
 * Resolve the persona speed, clamped to `[0.7, 1.2]` and rounded to 2 decimals.
 * Ports `resolveElevenLabsSpeed` (app.html line ~3332). Missing/non-finite
 * speed defaults to 1.0.
 */
export function resolveElevenLabsSpeed(persona: BrowserPersonaConfig | null | undefined): number {
  const speed = persona?.elevenlabs?.voiceSettings?.speed;
  return Math.round(clamp(Number.isFinite(speed) ? (speed as number) : 1.0, 0.7, 1.2) * 100) / 100;
}

/**
 * Map an output-format string to a MIME type. Ports `elevenLabsMimeType`
 * (app.html line ~3337): `wav*`→`audio/wav`, `pcm*`→`audio/pcm`,
 * `opus*`→`audio/opus`, else `audio/mpeg`.
 */
export function elevenLabsMimeType(outputFormat: string | null | undefined): string {
  if ((outputFormat || "").startsWith("wav")) return "audio/wav";
  if ((outputFormat || "").startsWith("pcm")) return "audio/pcm";
  if ((outputFormat || "").startsWith("opus")) return "audio/opus";
  return "audio/mpeg";
}

/**
 * Parse the sample rate from a `pcm_<rate>` output format, defaulting to 24000.
 * Ports `elevenLabsSampleRate` (app.html line ~3344).
 */
export function elevenLabsSampleRate(outputFormat: string | null | undefined): number {
  const match = /^pcm_(\d+)/i.exec(outputFormat || "");
  return match ? Number(match[1]) : 24000;
}

/**
 * Synthesize a single ElevenLabs request.
 *
 * Ports `synthesizeElevenLabsSingle` (app.html line ~3349): POSTs to
 * `{baseUrl}/v1/text-to-speech/{voiceId}?output_format=...` with the
 * `xi-api-key` header. Voice settings are the persona's, with `speed` overridden
 * by {@link resolveElevenLabsSpeed} (or `{ speed: 1.0 }` when absent).
 * `language_code` is included only when configured. PCM output is WAV-wrapped
 * unless `rawPcm` is set; otherwise the response is returned as a `Blob` typed
 * from the response `content-type` (falling back to {@link elevenLabsMimeType}).
 */
export async function synthesizeElevenLabsSingle(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  options: ElevenLabsSingleOptions = {},
): Promise<Blob> {
  options.throwIfCancelled?.();
  const elevenlabs = config.providers?.elevenlabs;
  if (!elevenlabs) throw new Error("ElevenLabs TTS is not configured.");
  const voiceId = persona?.elevenlabs?.voiceId;
  if (!voiceId) throw new Error("ElevenLabs voice_id is not configured for this persona.");
  const voiceSettings = persona?.elevenlabs?.voiceSettings
    ? { ...persona.elevenlabs.voiceSettings, speed: resolveElevenLabsSpeed(persona) }
    : { speed: 1.0 };
  const outputFormat = options.outputFormat || elevenlabs.outputFormat;
  const url = `${elevenlabs.baseUrl}/v1/text-to-speech/${encodeURIComponent(voiceId)}?output_format=${encodeURIComponent(outputFormat)}`;
  const body: Record<string, unknown> = {
    text: input,
    model_id: options.model ?? elevenlabs.modelId,
    voice_settings: voiceSettings,
    apply_text_normalization: elevenlabs.applyTextNormalization,
  };
  if (elevenlabs.languageCode) body.language_code = elevenlabs.languageCode;
  const timed = providerTimeoutSignal(elevenlabs.timeoutMs, input, options.signal);
  try {
    const response = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "xi-api-key": elevenlabs.apiKey,
      },
      body: JSON.stringify(body),
      signal: timed.signal,
    });
    if (!response.ok) throw await providerError(response, "ElevenLabs TTS failed");
    const bytes = await response.arrayBuffer();
    if ((outputFormat || "").startsWith("pcm") && !options.rawPcm) {
      return wavBlobFromPcm(new Uint8Array(bytes), elevenLabsSampleRate(outputFormat));
    }
    return new Blob([bytes], {
      type: response.headers.get("content-type") || elevenLabsMimeType(outputFormat),
    });
  } finally {
    timed.dispose();
  }
}

/**
 * Synthesize ElevenLabs audio, chunking long inputs.
 *
 * Ports `synthesizeElevenLabs` (app.html line ~3386): inputs of at least
 * {@link TTS_CHUNK_MIN_CHARS} codepoints that split into more than one chunk are
 * requested as raw `pcm_24000` via {@link synthesizeChunksOrdered} (pool limit
 * 3), concatenated with boundary silence, and WAV-wrapped at 24000 Hz.
 * Short/single-chunk inputs go through {@link synthesizeElevenLabsSingle} with
 * the configured output format.
 *
 * Note: the per-chunk model override defaults to the config `modelId`; pass
 * `options.model` (resolved from settings) to honor a user selection.
 */
export async function synthesizeElevenLabs(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  options: ElevenLabsSynthOptions = {},
): Promise<Blob> {
  if (Array.from(input).length >= TTS_CHUNK_MIN_CHARS) {
    const chunks = splitTtsText(input);
    if (chunks.length > 1) {
      const outputFormat = "pcm_24000";
      const parts = await synthesizeChunksOrdered(chunks, 3, async (chunk) => {
        options.throwIfCancelled?.();
        const blob = await synthesizeElevenLabsSingle(config, chunk, persona, {
          outputFormat,
          rawPcm: true,
          model: options.model,
          signal: options.signal,
          throwIfCancelled: options.throwIfCancelled,
        });
        return new Uint8Array(await blob.arrayBuffer());
      });
      const sampleRate = elevenLabsSampleRate(outputFormat);
      return wavBlobFromPcm(concatPcmChunksWithBoundarySilence(parts, sampleRate), sampleRate);
    }
  }
  return synthesizeElevenLabsSingle(config, input, persona, {
    model: options.model,
    signal: options.signal,
    throwIfCancelled: options.throwIfCancelled,
  });
}
