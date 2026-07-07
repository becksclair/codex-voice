/**
 * Pure waveform data extraction and massaging.
 *
 * Ports the data-producing parts of the waveform code in app.html; canvas
 * drawing (`drawWaveform`, `drawPeakWaveform`, etc.) stays in B2. Ported here:
 * - `audioContextCtor` (line ~1402)
 * - `peaksFromAudioBuffer` (line ~1254)
 * - the decode-to-peaks core of `decodeWaveformBlob` (line ~1274)
 * - the PCM bucketing of `appendStreamingWaveformPcm` (line ~1305)
 * - `samplePeaks` (line ~1171) and `peakContrastRange` (line ~1195), used to
 *   turn stored peaks into draw-ready bar heights
 */

import { clamp } from "../util.ts";

interface AudioContextConstructor {
  new (): AudioContext;
}

/**
 * Return the available `AudioContext` constructor, or `null`.
 *
 * Ports `audioContextCtor` (app.html line ~1402), including the legacy
 * `webkitAudioContext` fallback.
 */
export function audioContextCtor(): AudioContextConstructor | null {
  const w = window as typeof window & { webkitAudioContext?: AudioContextConstructor };
  return w.AudioContext || w.webkitAudioContext || null;
}

/**
 * Extract per-bucket peak amplitudes from a decoded `AudioBuffer`.
 *
 * Ports `peaksFromAudioBuffer` (app.html line ~1254): divides the buffer into
 * up to `targetCount` equal buckets (the last bucket absorbs the remainder) and
 * records the max absolute sample across all channels per bucket.
 */
export function peaksFromAudioBuffer(buffer: AudioBuffer, targetCount = 640): number[] {
  const length = buffer.length;
  const count = Math.max(1, Math.min(targetCount, length));
  const bucketSize = Math.max(1, Math.floor(length / count));
  const peaks: number[] = [];
  for (let bucket = 0; bucket < count; bucket += 1) {
    const start = bucket * bucketSize;
    const end = bucket === count - 1 ? length : Math.min(length, start + bucketSize);
    let peak = 0;
    for (let channel = 0; channel < buffer.numberOfChannels; channel += 1) {
      const data = buffer.getChannelData(channel);
      for (let index = start; index < end; index += 1) {
        peak = Math.max(peak, Math.abs(data[index] || 0));
      }
    }
    peaks.push(peak);
  }
  return peaks;
}

/** Peaks plus total duration decoded from an audio blob. */
export interface DecodedWaveform {
  peaks: number[];
  duration: number;
}

/**
 * Decode an audio blob and extract its waveform peaks and duration.
 *
 * Ports the pure core of `decodeWaveformBlob` (app.html line ~1274): decodes
 * via a fresh `AudioContext`, runs {@link peaksFromAudioBuffer}, and always
 * closes the context. Returns `null` when no `AudioContext` is available or the
 * blob is falsy (matching the original's `resetWaveform` early-out). The
 * decode-id race guarding and global state mutation stay in B2.
 */
export async function decodeAudioPeaks(
  blob: Blob | null | undefined,
  targetCount = 640,
): Promise<DecodedWaveform | null> {
  const Ctor = audioContextCtor();
  if (!Ctor || !blob) return null;
  let context: AudioContext | null = null;
  try {
    const bytes = await blob.arrayBuffer();
    context = new Ctor();
    const buffer = await context.decodeAudioData(bytes.slice(0));
    return { peaks: peaksFromAudioBuffer(buffer, targetCount), duration: buffer.duration };
  } finally {
    void context?.close?.().catch(() => {});
  }
}

/** Streaming peak bucketing result. */
export interface StreamingWaveformPeaks {
  peaks: number[];
  /** Seconds of audio represented, added to the buffered duration. */
  durationDelta: number;
}

/**
 * Extract streaming waveform peaks from a chunk of 16-bit PCM bytes.
 *
 * Ports the bucketing loop of `appendStreamingWaveformPcm` (app.html line
 * ~1305): buckets of `max(128, floor(sampleRate / 45))` frames, each yielding
 * the max normalized absolute sample across channels. `durationDelta` is
 * `frameCount / sampleRate`. Callers append `peaks` to their waveform state and
 * add `durationDelta` to the buffered duration.
 */
export function streamingPcmPeaks(
  bytes: Uint8Array,
  sampleRate: number,
  channels = 1,
): StreamingWaveformPeaks {
  const frameCount = Math.floor(bytes.length / (2 * channels));
  const bucketFrames = Math.max(128, Math.floor(sampleRate / 45));
  const peaks: number[] = [];
  for (let frame = 0; frame < frameCount; frame += bucketFrames) {
    const end = Math.min(frameCount, frame + bucketFrames);
    let peak = 0;
    for (let i = frame; i < end; i += 1) {
      for (let channel = 0; channel < channels; channel += 1) {
        const offset = (i * channels + channel) * 2;
        const value = bytes[offset] | (bytes[offset + 1] << 8);
        const signed = value >= 0x8000 ? value - 0x10000 : value;
        peak = Math.max(peak, Math.abs(signed / 32768));
      }
    }
    peaks.push(peak);
  }
  return { peaks, durationDelta: frameCount / sampleRate };
}

/**
 * Downsample stored peaks to exactly `count` composite values.
 *
 * Ports `samplePeaks` (app.html line ~1171): each output bucket blends the
 * mean (0.62), RMS (0.28), and max (0.10) of its source peaks, clamped to
 * `[0, 1]`. Returns `[]` for empty input or `count <= 0`.
 */
export function samplePeaks(peaks: readonly number[], count: number): number[] {
  if (!peaks?.length || count <= 0) return [];
  const sampled: number[] = [];
  for (let i = 0; i < count; i += 1) {
    const start = Math.floor((i / count) * peaks.length);
    const end = Math.max(start + 1, Math.ceil(((i + 1) / count) * peaks.length));
    let max = 0;
    let sum = 0;
    let sumSquares = 0;
    let samples = 0;
    for (let j = start; j < end && j < peaks.length; j += 1) {
      const peak = clamp(peaks[j] || 0, 0, 1);
      max = Math.max(max, peak);
      sum += peak;
      sumSquares += peak * peak;
      samples += 1;
    }
    const mean = samples > 0 ? sum / samples : 0;
    const rms = samples > 0 ? Math.sqrt(sumSquares / samples) : 0;
    sampled.push(clamp(mean * 0.62 + rms * 0.28 + max * 0.1, 0, 1));
  }
  return sampled;
}

/** Contrast floor/ceiling used to stretch peak amplitudes for drawing. */
export interface PeakContrastRange {
  floor: number;
  ceiling: number;
}

/**
 * Compute a robust min/max contrast window over peaks.
 *
 * Ports `peakContrastRange` (app.html line ~1195): sorts finite, clamped peaks
 * and reads the 12th and 90th percentiles, then pads the floor down by 0.025
 * and forces the ceiling at least 0.18 above the floor. Returns
 * `{ floor: 0, ceiling: 1 }` for empty/degenerate input.
 */
export function peakContrastRange(peaks: readonly number[]): PeakContrastRange {
  if (!peaks?.length) return { floor: 0, ceiling: 1 };
  const sorted = peaks
    .filter((peak) => Number.isFinite(peak))
    .map((peak) => clamp(peak, 0, 1))
    .sort((a, b) => a - b);
  if (!sorted.length) return { floor: 0, ceiling: 1 };
  const low = sorted[Math.floor((sorted.length - 1) * 0.12)];
  const high = sorted[Math.floor((sorted.length - 1) * 0.9)];
  return {
    floor: Math.max(0, low - 0.025),
    ceiling: Math.min(1, Math.max(high, low + 0.18)),
  };
}
