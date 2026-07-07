/**
 * Browser-direct Google TTS client.
 *
 * Ports the non-streaming Google path from app.html:
 * - `normalizeGoogleModelName` (line ~1848)
 * - `resolveGoogleModel` (line ~1892)
 * - `buildGoogleTtsPrompt` (line ~2767)
 * - `canStreamGoogle` (line ~2993)
 * - `fetchGoogleAudio` (line ~3253)
 * - `synthesizeGoogle` (line ~3306)
 *
 * The `interactions`-stream playback path (`streamGoogle`,
 * `readGoogleInteractionStream`) is UI/AudioContext-playback orchestration and
 * is intentionally left to B2; see the module docs. Request bodies, headers,
 * and response parsing here are byte-for-byte faithful to the live API calls.
 */

import { audioContextCtor } from "../audio/waveform.ts";
import {
  concatPcmChunksWithBoundarySilence,
  concatWavChunksWithBoundarySilence,
  parseSampleRate,
  wavBlobFromPcm,
} from "../audio/wav.ts";
import type { BrowserGoogleConfig, BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import { TTS_CHUNK_MIN_CHARS, splitTtsText } from "./chunking.ts";
import { providerError, selectedProviderModel } from "./common.ts";
import { synthesizeChunksOrdered } from "./pool.ts";

/** Options shared by the Google synth entry points. */
export interface GoogleSynthOptions {
  /**
   * Model to request. Defaults to the config's `google.model`. B2 should pass
   * `resolveGoogleModel(google, settings.model)` to honor the user's choice.
   */
  model?: string;
  /** Abort signal forwarded to `fetch`. */
  signal?: AbortSignal | null;
  /**
   * Cancellation check invoked before each chunk fetch, mirroring
   * `throwIfGenerationCancelled` in the original. Should throw to abort.
   */
  throwIfCancelled?: () => void;
}

/** Strip a leading `google/` from a model name. Ports `normalizeGoogleModelName`. */
export function normalizeGoogleModelName(model: string | null | undefined): string {
  return String(model || "").replace(/^google\//, "");
}

/**
 * Resolve the Google model from the settings `model` value.
 *
 * Ports `resolveGoogleModel` (app.html line ~1892). Returns `''` when `google`
 * is absent; otherwise delegates to `selectedProviderModel` with the config
 * default `google.model`.
 */
export function resolveGoogleModel(
  google: BrowserGoogleConfig | null | undefined,
  settingsModel?: string | null,
): string {
  if (!google) return "";
  return selectedProviderModel(settingsModel, "google", google.model);
}

/**
 * Build the Google TTS instruction prompt.
 *
 * Ports `buildGoogleTtsPrompt` (app.html line ~2767): assembles a delivery
 * profile from the persona's scene/style/pace/constraints and optional sample
 * context, appends any extra `instructions`, the fixed "speak exactly as
 * written" guardrails, and finally the input wrapped in triple quotes. The
 * string layout (headings, bullet order, blank lines) is preserved verbatim.
 */
export function buildGoogleTtsPrompt(
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  instructions: string | null | undefined,
): string {
  let prompt = "Read the following text aloud.\n\n";
  if (persona) {
    prompt += "Delivery profile:\n";
    if (persona.promptScene) prompt += `- scene: ${persona.promptScene}\n`;
    if (persona.promptStyle) prompt += `- style: ${persona.promptStyle}\n`;
    if (persona.promptPacing) prompt += `- pace: ${persona.promptPacing}\n`;
    for (const constraint of persona.promptConstraints || []) {
      prompt += `- constraint: ${constraint}\n`;
    }
    prompt += "\n";
    if (persona.promptSampleContext) {
      prompt += `Sample context: ${persona.promptSampleContext}\n\n`;
    }
  }
  if (instructions) {
    prompt += "Additional delivery hints:\n";
    prompt += `- ${instructions}\n\n`;
  }
  prompt += "Important:\n";
  prompt += "- speak the text exactly as written\n";
  prompt += "- do not add narration or commentary\n";
  prompt += "- do not change wording or paraphrase\n\n";
  prompt += `Text:\n"""${input}"""`;
  return prompt;
}

/**
 * Whether Google streaming is usable for the currently-selected model.
 *
 * Ports `canStreamGoogle` (app.html line ~2993): requires the provider config,
 * an `AudioContext`, `ReadableStream`, and the normalized selected model to be
 * listed in `google.streaming.supportedModels`. `settingsModel` is threaded
 * through model resolution.
 */
export function canStreamGoogle(
  config: BrowserTtsConfig | null | undefined,
  settingsModel?: string | null,
): boolean {
  const google = config?.providers?.google;
  if (!google || !audioContextCtor() || !window.ReadableStream) return false;
  const selected = normalizeGoogleModelName(resolveGoogleModel(google, settingsModel));
  return (google.streaming?.supportedModels || []).includes(selected);
}

/** Raw audio bytes plus MIME type returned by a single Google request. */
export interface GoogleAudio {
  bytes: Uint8Array;
  mimeType: string;
}

/**
 * Fetch a single Google TTS response.
 *
 * Ports `fetchGoogleAudio` (app.html line ~3253): POSTs to
 * `{baseUrl}/models/{model}:generateContent` with the `x-goog-api-key` header
 * and an `AUDIO` response modality, then extracts the first inline audio part.
 * Throws a {@link ProviderError} on a non-OK response and a plain error when no
 * audio is present. The MIME type falls back to
 * `'audio/L16;codec=pcm;rate=24000'`.
 */
export async function fetchGoogleAudio(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  instructions: string | null | undefined,
  options: GoogleSynthOptions = {},
): Promise<GoogleAudio> {
  const google = config.providers?.google;
  if (!google) throw new Error("Google TTS is not configured.");
  const model = options.model ?? google.model;
  const voiceName = persona?.google?.voiceName || google.voice;
  const response = await fetch(
    `${google.baseUrl}/models/${encodeURIComponent(model)}:generateContent`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "x-goog-api-key": google.apiKey,
      },
      body: JSON.stringify({
        contents: [
          { role: "user", parts: [{ text: buildGoogleTtsPrompt(input, persona, instructions) }] },
        ],
        generationConfig: {
          responseModalities: ["AUDIO"],
          speechConfig: {
            voiceConfig: {
              prebuiltVoiceConfig: { voiceName },
            },
          },
        },
      }),
      signal: options.signal ?? null,
    },
  );
  if (!response.ok) throw await providerError(response, "Google TTS failed");
  const json = (await response.json()) as {
    candidates?: { content?: { parts?: GoogleInlinePart[] } }[];
  };
  const parts = json?.candidates?.[0]?.content?.parts || [];
  const inline = parts.map((part) => part.inlineData || part.inline_data).find(Boolean);
  if (!inline?.data) throw new Error("Google TTS returned no audio.");
  const mimeType = inline.mimeType || inline.mime_type || "audio/L16;codec=pcm;rate=24000";
  const bytes = base64ToBytes(inline.data);
  return { bytes, mimeType };
}

interface GoogleInlineData {
  data?: string;
  mimeType?: string;
  mime_type?: string;
}

interface GoogleInlinePart {
  inlineData?: GoogleInlineData;
  inline_data?: GoogleInlineData;
}

// Local base64 decode (kept here to match app.html's `bytesFromBase64` inlined
// into the Google path). Identical byte behavior to audio/wav.ts.
function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function isPcmMime(mimeType: string): boolean {
  const lower = (mimeType || "").toLowerCase();
  return lower.startsWith("audio/l16") || lower.startsWith("audio/pcm");
}

/**
 * Synthesize Google TTS audio, chunking long inputs.
 *
 * Ports `synthesizeGoogle` (app.html line ~3306). When the input has at least
 * {@link TTS_CHUNK_MIN_CHARS} codepoints and splits into more than one chunk,
 * chunks are fetched via {@link synthesizeChunksOrdered} with a pool limit of
 * 3 and stitched: all-PCM chunks are concatenated with boundary silence and
 * wrapped as WAV; all-WAV chunks are stitched via
 * {@link concatWavChunksWithBoundarySilence}; mixed results fall back to a raw
 * `Blob`. Short/single-chunk inputs make one request and are WAV-wrapped when
 * PCM, else returned raw.
 */
export async function synthesizeGoogle(
  config: BrowserTtsConfig,
  input: string,
  persona: BrowserPersonaConfig | null | undefined,
  instructions: string | null | undefined,
  options: GoogleSynthOptions = {},
): Promise<Blob> {
  if (Array.from(input).length >= TTS_CHUNK_MIN_CHARS) {
    const chunks = splitTtsText(input);
    if (chunks.length > 1) {
      const audios = await synthesizeChunksOrdered(chunks, 3, (chunk) => {
        options.throwIfCancelled?.();
        return fetchGoogleAudio(config, chunk, persona, instructions, options);
      });
      const mimeType = audios[0].mimeType || "audio/L16;codec=pcm;rate=24000";
      const sampleRate = parseSampleRate(mimeType);
      if (audios.every((audio) => isPcmMime(audio.mimeType))) {
        return wavBlobFromPcm(
          concatPcmChunksWithBoundarySilence(
            audios.map((audio) => audio.bytes),
            sampleRate,
          ),
          sampleRate,
        );
      }
      if (audios.every((audio) => (audio.mimeType || "").toLowerCase().startsWith("audio/wav"))) {
        return concatWavChunksWithBoundarySilence(audios.map((audio) => audio.bytes));
      }
      return new Blob(
        audios.map((audio) => audio.bytes as BlobPart),
        { type: mimeType },
      );
    }
  }
  const { bytes, mimeType } = await fetchGoogleAudio(config, input, persona, instructions, options);
  if (isPcmMime(mimeType)) {
    return wavBlobFromPcm(bytes, parseSampleRate(mimeType));
  }
  return new Blob([bytes as BlobPart], { type: mimeType });
}
