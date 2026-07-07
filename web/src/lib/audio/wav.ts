/**
 * PCM/WAV assembly and base64 audio helpers.
 *
 * Ports the WAV-related functions from app.html:
 * - `bytesFromBase64`/`audioBlobFromBase64` (lines ~1835-1846)
 * - `parseSampleRate` (line ~2794)
 * - `writeAscii`/`wavBlobFromPcm` (lines ~2799-2823)
 * - `concatUint8Arrays`/`pcmBoundarySilence`/`concatPcmChunksWithBoundarySilence`
 *   (lines ~2853-2878)
 * - `asciiFromBytes`/`wavPcmData`/`concatWavChunksWithBoundarySilence`
 *   (lines ~2880-2934)
 *
 * Byte layout, the odd-`size` padding in RIFF chunk walking, and the trailing
 * odd-byte handling in gain/concat paths are preserved exactly.
 */

import { TTS_CHUNK_BOUNDARY_SILENCE_MS } from "../synth/chunking.ts";

/** Decode a base64 string to raw bytes. Ports `bytesFromBase64`. */
export function bytesFromBase64(base64Audio: string): Uint8Array {
  const binary = atob(base64Audio);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Build an audio `Blob` from base64 bytes. Ports `audioBlobFromBase64`;
 * `mimeType` defaults to `'audio/wav'`.
 */
export function audioBlobFromBase64(base64Audio: string, mimeType?: string): Blob {
  return new Blob([bytesFromBase64(base64Audio) as BlobPart], { type: mimeType || "audio/wav" });
}

/**
 * Parse a `rate=<n>` sample rate from a MIME type, defaulting to 24000.
 * Ports `parseSampleRate` (app.html line ~2794).
 */
export function parseSampleRate(mimeType: string | null | undefined): number {
  const match = /rate=(\d+)/i.exec(mimeType || "");
  return match ? Number(match[1]) : 24000;
}

/** Write an ASCII string into a DataView at `offset`. Ports `writeAscii`. */
export function writeAscii(view: DataView, offset: number, value: string): void {
  for (let i = 0; i < value.length; i += 1) {
    view.setUint8(offset + i, value.charCodeAt(i));
  }
}

/**
 * Wrap raw 16-bit PCM bytes in a 44-byte WAV/RIFF header.
 *
 * Ports `wavBlobFromPcm` (app.html line ~2805). `blockAlign = channels * 2`
 * (16-bit samples). The resulting blob has type `audio/wav`.
 */
export function wavBlobFromPcm(pcmBytes: Uint8Array, sampleRate: number, channels = 1): Blob {
  const header = new ArrayBuffer(44);
  const view = new DataView(header);
  const blockAlign = channels * 2;
  writeAscii(view, 0, "RIFF");
  view.setUint32(4, 36 + pcmBytes.length, true);
  writeAscii(view, 8, "WAVE");
  writeAscii(view, 12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, channels, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * blockAlign, true);
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, 16, true);
  writeAscii(view, 36, "data");
  view.setUint32(40, pcmBytes.length, true);
  return new Blob([header, pcmBytes as BlobPart], { type: "audio/wav" });
}

/** Concatenate byte arrays into one. Ports `concatUint8Arrays`. */
export function concatUint8Arrays(parts: readonly Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const output = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.length;
  }
  return output;
}

/**
 * A block of silent 16-bit PCM for the inter-chunk boundary.
 *
 * Ports `pcmBoundarySilence` (app.html line ~2864): length is
 * `floor(sampleRate * TTS_CHUNK_BOUNDARY_SILENCE_MS / 1000)` frames of zeroed
 * 16-bit samples per channel.
 */
export function pcmBoundarySilence(sampleRate: number, channels = 1): Uint8Array {
  const frames = Math.floor((sampleRate * TTS_CHUNK_BOUNDARY_SILENCE_MS) / 1000);
  return new Uint8Array(frames * channels * 2);
}

/**
 * Concatenate PCM chunks, inserting {@link pcmBoundarySilence} between them.
 *
 * Ports `concatPcmChunksWithBoundarySilence` (app.html line ~2869). A single
 * (or empty) chunk list is concatenated with no silence.
 */
export function concatPcmChunksWithBoundarySilence(
  parts: readonly Uint8Array[],
  sampleRate: number,
  channels = 1,
): Uint8Array {
  if (parts.length <= 1) return concatUint8Arrays(parts);
  const silence = pcmBoundarySilence(sampleRate, channels);
  const interleaved: Uint8Array[] = [];
  parts.forEach((part, index) => {
    if (index > 0) interleaved.push(silence);
    interleaved.push(part);
  });
  return concatUint8Arrays(interleaved);
}

/** Read `length` bytes at `offset` as an ASCII string. Ports `asciiFromBytes`. */
export function asciiFromBytes(bytes: Uint8Array, offset: number, length: number): string {
  let value = "";
  for (let i = 0; i < length; i += 1) value += String.fromCharCode(bytes[offset + i]);
  return value;
}

/** Parsed `fmt ` chunk plus the raw PCM `data` payload of a WAV. */
export interface WavPcmData {
  audioFormat: number;
  channels: number;
  sampleRate: number;
  bitsPerSample: number;
  data: Uint8Array;
}

/**
 * Parse a 16-bit PCM WAV blob into its format fields and data bytes.
 *
 * Ports `wavPcmData` (app.html line ~2886). Walks RIFF chunks from offset 12,
 * advancing by `body + size + (size % 2)` to honor odd-size padding, and stops
 * on a truncated chunk. Throws on: length < 44, bad RIFF/WAVE magic, missing
 * `fmt `/`data`, or a non-PCM / non-16-bit format — with the exact legacy error
 * strings.
 */
export function wavPcmData(bytes: Uint8Array): WavPcmData {
  if (bytes.length < 44) throw new Error("WAV chunk is too small.");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (asciiFromBytes(bytes, 0, 4) !== "RIFF" || asciiFromBytes(bytes, 8, 4) !== "WAVE") {
    throw new Error("WAV chunk has invalid RIFF/WAVE header.");
  }
  let offset = 12;
  let format: Omit<WavPcmData, "data"> | null = null;
  let data: Uint8Array | null = null;
  while (offset + 8 <= bytes.length) {
    const id = asciiFromBytes(bytes, offset, 4);
    const size = view.getUint32(offset + 4, true);
    const body = offset + 8;
    if (body + size > bytes.length) break;
    if (id === "fmt ") {
      format = {
        audioFormat: view.getUint16(body, true),
        channels: view.getUint16(body + 2, true),
        sampleRate: view.getUint32(body + 4, true),
        bitsPerSample: view.getUint16(body + 14, true),
      };
    } else if (id === "data") {
      data = bytes.slice(body, body + size);
    }
    offset = body + size + (size % 2);
  }
  if (!format || !data) throw new Error("WAV chunk is missing fmt or data.");
  if (format.audioFormat !== 1 || format.bitsPerSample !== 16) {
    throw new Error("Only 16-bit PCM WAV chunks can be stitched.");
  }
  return { ...format, data };
}

/**
 * Stitch multiple WAV chunks into one, with boundary silence between them.
 *
 * Ports `concatWavChunksWithBoundarySilence` (app.html line ~2919). Requires
 * matching sample rate and channel counts across chunks (throws otherwise),
 * concatenates the decoded PCM via {@link concatPcmChunksWithBoundarySilence},
 * and re-wraps it with {@link wavBlobFromPcm}.
 */
export function concatWavChunksWithBoundarySilence(parts: readonly Uint8Array[]): Blob {
  const decoded = parts.map(wavPcmData);
  const first = decoded[0];
  if (!first) throw new Error("No WAV chunks to stitch.");
  for (const chunk of decoded) {
    if (chunk.sampleRate !== first.sampleRate || chunk.channels !== first.channels) {
      throw new Error("WAV chunks have mismatched sample rates or channels.");
    }
  }
  const pcm = concatPcmChunksWithBoundarySilence(
    decoded.map((chunk) => chunk.data),
    first.sampleRate,
    first.channels,
  );
  return wavBlobFromPcm(pcm, first.sampleRate, first.channels);
}
