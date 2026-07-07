import { describe, expect, it } from "vitest";
import {
  peakContrastRange,
  peaksFromAudioBuffer,
  samplePeaks,
  streamingPcmPeaks,
} from "./waveform.ts";

/** Minimal AudioBuffer stand-in for peaksFromAudioBuffer. */
function fakeBuffer(channels: number[][]): AudioBuffer {
  return {
    length: channels[0].length,
    numberOfChannels: channels.length,
    getChannelData: (channel: number) => Float32Array.from(channels[channel]),
  } as unknown as AudioBuffer;
}

describe("peaksFromAudioBuffer", () => {
  it("buckets max absolute amplitude", () => {
    const buffer = fakeBuffer([[0, 0.5, -0.9, 0.2]]);
    const peaks = peaksFromAudioBuffer(buffer, 2);
    // Two buckets of 2 samples: max(|0|,|0.5|)=0.5, max(|-0.9|,|0.2|)=0.9.
    // Float32 storage introduces tiny rounding, so compare approximately.
    expect(peaks[0]).toBeCloseTo(0.5, 5);
    expect(peaks[1]).toBeCloseTo(0.9, 5);
  });

  it("takes the max across channels", () => {
    const buffer = fakeBuffer([
      [0.1, 0.1],
      [0.4, 0.4],
    ]);
    const peaks = peaksFromAudioBuffer(buffer, 1);
    expect(peaks[0]).toBeCloseTo(0.4, 5);
  });

  it("caps the count at the buffer length", () => {
    const buffer = fakeBuffer([[0.2, 0.4]]);
    expect(peaksFromAudioBuffer(buffer, 640)).toHaveLength(2);
  });
});

describe("streamingPcmPeaks", () => {
  it("extracts a normalized peak and duration delta", () => {
    // frame0 = 0x4000 (16384) -> 0.5; frame1 = 0x8000 -> -32768 -> abs 1.0.
    const bytes = new Uint8Array([0x00, 0x40, 0x00, 0x80]);
    const result = streamingPcmPeaks(bytes, 24000, 1);
    // Both frames fall in one bucket (bucketFrames >> 2).
    expect(result.peaks).toEqual([1]);
    expect(result.durationDelta).toBeCloseTo(2 / 24000, 10);
  });

  it("returns no peaks for an empty buffer", () => {
    expect(streamingPcmPeaks(new Uint8Array(), 24000, 1)).toEqual({ peaks: [], durationDelta: 0 });
  });
});

describe("samplePeaks", () => {
  it("returns [] for empty peaks or non-positive count", () => {
    expect(samplePeaks([], 4)).toEqual([]);
    expect(samplePeaks([0.5], 0)).toEqual([]);
  });

  it("blends mean/rms/max and clamps to [0,1]", () => {
    // Single uniform value collapses mean=rms=max=v, blend = v*(0.62+0.28+0.1)=v.
    const out = samplePeaks([0.4, 0.4, 0.4, 0.4], 1);
    expect(out).toHaveLength(1);
    expect(out[0]).toBeCloseTo(0.4, 10);
  });

  it("produces the requested number of buckets", () => {
    expect(samplePeaks([0.1, 0.2, 0.3, 0.4, 0.5, 0.6], 3)).toHaveLength(3);
  });
});

describe("peakContrastRange", () => {
  it("returns the full range for empty input", () => {
    expect(peakContrastRange([])).toEqual({ floor: 0, ceiling: 1 });
  });

  it("pads the floor down and enforces a minimum ceiling gap", () => {
    const range = peakContrastRange([0.5, 0.5, 0.5, 0.5]);
    // low = high = 0.5 -> floor = 0.475, ceiling = max(0.5, 0.5+0.18) = 0.68.
    expect(range.floor).toBeCloseTo(0.475, 10);
    expect(range.ceiling).toBeCloseTo(0.68, 10);
  });

  it("ignores non-finite peaks", () => {
    const range = peakContrastRange([Number.NaN, 0.2, 0.8]);
    expect(Number.isFinite(range.floor)).toBe(true);
    expect(Number.isFinite(range.ceiling)).toBe(true);
  });
});
