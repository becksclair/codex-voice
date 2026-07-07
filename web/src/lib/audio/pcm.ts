/**
 * Pure 16-bit PCM sample helpers used by streaming playback.
 *
 * Ports `ttsStreamPcmGain`, `clampPcm16`, `applyPcm16Gain`, `evenPcmBytes`, and
 * `pcm16ToAudioBuffer` from app.html (lines ~1452-1509). The `StreamingPlayback`
 * class and stream sink that consume these are DOM/AudioContext-playback
 * orchestration and are left to B2; only the byte-level transforms live here.
 */

import type { BrowserTtsConfig } from "../config.ts";

/**
 * Resolve the PCM gain multiplier from config.
 *
 * Ports `ttsStreamPcmGain` (app.html line ~1452): uses
 * `providers.elevenlabs.streamGain` when it is a finite positive number, else
 * defaults to `2.0`.
 */
export function ttsStreamPcmGain(config: BrowserTtsConfig | null | undefined): number {
  const gain = Number(config?.providers?.elevenlabs?.streamGain);
  return Number.isFinite(gain) && gain > 0 ? gain : 2.0;
}

/** Clamp/round a value into signed 16-bit range. Ports `clampPcm16`. */
export function clampPcm16(value: number): number {
  return Math.max(-32768, Math.min(32767, Math.round(value)));
}

/**
 * Apply a gain multiplier to little-endian 16-bit PCM bytes.
 *
 * Ports `applyPcm16Gain` (app.html line ~1461). Returns the input unchanged
 * when gain is non-finite, exactly 1, or the buffer is empty. A trailing odd
 * byte (incomplete sample) is copied through verbatim.
 */
export function applyPcm16Gain(bytes: Uint8Array, gain: number): Uint8Array {
  if (!Number.isFinite(gain) || gain === 1 || !bytes?.length) return bytes;
  const output = new Uint8Array(bytes.length);
  for (let offset = 0; offset + 1 < bytes.length; offset += 2) {
    const value = bytes[offset] | (bytes[offset + 1] << 8);
    const signed = value >= 0x8000 ? value - 0x10000 : value;
    const amplified = clampPcm16(signed * gain);
    output[offset] = amplified & 0xff;
    output[offset + 1] = (amplified >> 8) & 0xff;
  }
  if (bytes.length % 2 === 1) output[bytes.length - 1] = bytes[bytes.length - 1];
  return output;
}

/** Mutable carry cell for a trailing odd byte across streamed PCM chunks. */
export interface PendingByte {
  value: number | null;
}

/**
 * Return an even-length PCM slice, buffering any trailing odd byte in `pending`.
 *
 * Ports `evenPcmBytes` (app.html line ~1475). A leftover byte from a previous
 * call (`pending.value`) is prepended; if the combined length is odd, the final
 * byte is stashed back into `pending.value` for the next call. When there are
 * not enough bytes to emit a full sample, an empty array is returned and the
 * pending byte is retained/seeded.
 */
export function evenPcmBytes(bytes: Uint8Array, pending: PendingByte): Uint8Array {
  const hasPending = pending.value !== null;
  const length = bytes.length + (hasPending ? 1 : 0);
  const evenLength = length - (length % 2);
  if (evenLength === 0) {
    pending.value = hasPending ? pending.value : bytes[0];
    return new Uint8Array();
  }
  const output = new Uint8Array(evenLength);
  let outputOffset = 0;
  if (hasPending) {
    output[0] = pending.value as number;
    outputOffset = 1;
    pending.value = null;
  }
  const copyLength = evenLength - outputOffset;
  output.set(bytes.slice(0, copyLength), outputOffset);
  if (copyLength < bytes.length) pending.value = bytes[copyLength];
  return output;
}

/**
 * Decode little-endian 16-bit PCM bytes into an `AudioBuffer`.
 *
 * Ports `pcm16ToAudioBuffer` (app.html line ~1496). Frames are interleaved by
 * channel; each sample is normalized to `[-1, 1)` by dividing by 32768.
 */
export function pcm16ToAudioBuffer(
  context: BaseAudioContext,
  bytes: Uint8Array,
  sampleRate: number,
  channels = 1,
): AudioBuffer {
  const frameCount = Math.floor(bytes.length / (2 * channels));
  const buffer = context.createBuffer(channels, frameCount, sampleRate);
  for (let channel = 0; channel < channels; channel += 1) {
    const output = buffer.getChannelData(channel);
    for (let frame = 0; frame < frameCount; frame += 1) {
      const offset = (frame * channels + channel) * 2;
      const value = bytes[offset] | (bytes[offset + 1] << 8);
      const signed = value >= 0x8000 ? value - 0x10000 : value;
      output[frame] = signed / 32768;
    }
  }
  return buffer;
}
