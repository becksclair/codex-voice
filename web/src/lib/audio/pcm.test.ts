import { describe, expect, it } from "vitest";
import type { BrowserTtsConfig } from "../config.ts";
import { applyPcm16Gain, clampPcm16, evenPcmBytes, ttsStreamPcmGain } from "./pcm.ts";

describe("ttsStreamPcmGain", () => {
  it("defaults to 2.0 without a configured gain", () => {
    expect(ttsStreamPcmGain(null)).toBe(2.0);
    expect(ttsStreamPcmGain({} as BrowserTtsConfig)).toBe(2.0);
  });

  it("uses a finite positive configured gain", () => {
    const config = {
      providers: { elevenlabs: { streamGain: 1.5 } },
    } as unknown as BrowserTtsConfig;
    expect(ttsStreamPcmGain(config)).toBe(1.5);
  });

  it("ignores non-positive configured gains", () => {
    const config = { providers: { elevenlabs: { streamGain: 0 } } } as unknown as BrowserTtsConfig;
    expect(ttsStreamPcmGain(config)).toBe(2.0);
  });
});

describe("clampPcm16", () => {
  it("clamps and rounds to signed 16-bit range", () => {
    expect(clampPcm16(40000)).toBe(32767);
    expect(clampPcm16(-40000)).toBe(-32768);
    expect(clampPcm16(1.4)).toBe(1);
  });
});

describe("applyPcm16Gain", () => {
  it("returns input unchanged for gain === 1", () => {
    const bytes = new Uint8Array([1, 2, 3, 4]);
    expect(applyPcm16Gain(bytes, 1)).toBe(bytes);
  });

  it("returns input unchanged for empty buffer", () => {
    const bytes = new Uint8Array();
    expect(applyPcm16Gain(bytes, 2)).toBe(bytes);
  });

  it("amplifies a little-endian 16-bit sample", () => {
    // sample = 0x0100 = 256, gain 2 -> 512 = 0x0200.
    const out = applyPcm16Gain(new Uint8Array([0x00, 0x01]), 2);
    expect([...out]).toEqual([0x00, 0x02]);
  });

  it("clamps overflow to the 16-bit max", () => {
    // sample = 20000, gain 2 -> 40000 clamps to 32767 = 0x7FFF.
    const lo = 20000 & 0xff;
    const hi = (20000 >> 8) & 0xff;
    const out = applyPcm16Gain(new Uint8Array([lo, hi]), 2);
    expect([...out]).toEqual([0xff, 0x7f]);
  });

  it("copies a trailing odd byte through verbatim", () => {
    const out = applyPcm16Gain(new Uint8Array([0x00, 0x01, 0x42]), 2);
    expect(out.length).toBe(3);
    expect(out[2]).toBe(0x42);
  });
});

describe("evenPcmBytes", () => {
  it("passes even input through and leaves no pending byte", () => {
    const pending = { value: null as number | null };
    const out = evenPcmBytes(new Uint8Array([1, 2, 3, 4]), pending);
    expect([...out]).toEqual([1, 2, 3, 4]);
    expect(pending.value).toBeNull();
  });

  it("stashes a trailing odd byte for the next call", () => {
    const pending = { value: null as number | null };
    const out = evenPcmBytes(new Uint8Array([1, 2, 3]), pending);
    expect([...out]).toEqual([1, 2]);
    expect(pending.value).toBe(3);
  });

  it("prepends a pending byte on the following call", () => {
    const pending = { value: 9 as number | null };
    const out = evenPcmBytes(new Uint8Array([1, 2, 3]), pending);
    // 9 prepended + [1,2,3] = length 4 (even), nothing left pending.
    expect([...out]).toEqual([9, 1, 2, 3]);
    expect(pending.value).toBeNull();
  });

  it("returns empty and seeds pending when a single byte cannot form a sample", () => {
    const pending = { value: null as number | null };
    const out = evenPcmBytes(new Uint8Array([7]), pending);
    expect(out.length).toBe(0);
    expect(pending.value).toBe(7);
  });
});
