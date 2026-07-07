import { describe, expect, it } from "vitest";
import {
  asciiFromBytes,
  audioBlobFromBase64,
  bytesFromBase64,
  concatPcmChunksWithBoundarySilence,
  concatUint8Arrays,
  concatWavChunksWithBoundarySilence,
  parseSampleRate,
  pcmBoundarySilence,
  wavBlobFromPcm,
  wavPcmData,
} from "./wav.ts";

async function blobBytes(blob: Blob): Promise<Uint8Array> {
  return new Uint8Array(await blob.arrayBuffer());
}

function u32(bytes: Uint8Array, offset: number): number {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(offset, true);
}
function u16(bytes: Uint8Array, offset: number): number {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint16(offset, true);
}

describe("bytesFromBase64 / audioBlobFromBase64", () => {
  it("decodes base64 to bytes", () => {
    // "AQIDBA==" is [1,2,3,4].
    expect([...bytesFromBase64("AQIDBA==")]).toEqual([1, 2, 3, 4]);
  });

  it("defaults the blob mime type to audio/wav", () => {
    expect(audioBlobFromBase64("AQIDBA==").type).toBe("audio/wav");
    expect(audioBlobFromBase64("AQIDBA==", "audio/mpeg").type).toBe("audio/mpeg");
  });
});

describe("parseSampleRate", () => {
  it("reads rate=<n> and defaults to 24000", () => {
    expect(parseSampleRate("audio/L16;codec=pcm;rate=16000")).toBe(16000);
    expect(parseSampleRate("audio/wav")).toBe(24000);
    expect(parseSampleRate(null)).toBe(24000);
  });
});

describe("wavBlobFromPcm", () => {
  it("writes a correct 44-byte RIFF/fmt/data header", async () => {
    const pcm = new Uint8Array([1, 2, 3, 4]);
    const bytes = await blobBytes(wavBlobFromPcm(pcm, 24000, 1));
    expect(bytes.length).toBe(44 + 4);
    expect(asciiFromBytes(bytes, 0, 4)).toBe("RIFF");
    expect(u32(bytes, 4)).toBe(36 + 4);
    expect(asciiFromBytes(bytes, 8, 4)).toBe("WAVE");
    expect(asciiFromBytes(bytes, 12, 4)).toBe("fmt ");
    expect(u32(bytes, 16)).toBe(16); // fmt chunk size
    expect(u16(bytes, 20)).toBe(1); // PCM
    expect(u16(bytes, 22)).toBe(1); // channels
    expect(u32(bytes, 24)).toBe(24000); // sample rate
    expect(u32(bytes, 28)).toBe(48000); // byte rate = rate * blockAlign
    expect(u16(bytes, 32)).toBe(2); // block align = channels * 2
    expect(u16(bytes, 34)).toBe(16); // bits per sample
    expect(asciiFromBytes(bytes, 36, 4)).toBe("data");
    expect(u32(bytes, 40)).toBe(4); // data size
    expect([...bytes.slice(44)]).toEqual([1, 2, 3, 4]);
  });

  it("uses stereo block align", async () => {
    const bytes = await blobBytes(wavBlobFromPcm(new Uint8Array(8), 48000, 2));
    expect(u16(bytes, 22)).toBe(2); // channels
    expect(u16(bytes, 32)).toBe(4); // block align = 2 * 2
    expect(u32(bytes, 28)).toBe(48000 * 4); // byte rate
  });
});

describe("concat helpers", () => {
  it("concatUint8Arrays joins in order", () => {
    expect([...concatUint8Arrays([new Uint8Array([1, 2]), new Uint8Array([3])])]).toEqual([
      1, 2, 3,
    ]);
  });

  it("pcmBoundarySilence sizes 180ms of 16-bit mono", () => {
    // floor(24000 * 180 / 1000) = 4320 frames * 1 ch * 2 bytes.
    expect(pcmBoundarySilence(24000, 1).length).toBe(4320 * 2);
    expect(pcmBoundarySilence(24000, 2).length).toBe(4320 * 2 * 2);
  });

  it("concatPcmChunksWithBoundarySilence inserts silence only between parts", () => {
    const a = new Uint8Array([1, 2, 3, 4]);
    const b = new Uint8Array([5, 6, 7, 8]);
    const silence = pcmBoundarySilence(24000, 1).length;
    const joined = concatPcmChunksWithBoundarySilence([a, b], 24000, 1);
    expect(joined.length).toBe(a.length + silence + b.length);
    // No leading/trailing silence: first and last bytes come from the parts.
    expect(joined[0]).toBe(1);
    expect(joined[joined.length - 1]).toBe(8);
  });

  it("concatPcmChunksWithBoundarySilence skips silence for a single part", () => {
    const a = new Uint8Array([1, 2, 3, 4]);
    expect(concatPcmChunksWithBoundarySilence([a], 24000, 1).length).toBe(4);
  });
});

describe("wavPcmData", () => {
  it("round-trips a wavBlobFromPcm buffer", async () => {
    const pcm = new Uint8Array([10, 20, 30, 40]);
    const bytes = await blobBytes(wavBlobFromPcm(pcm, 16000, 1));
    const parsed = wavPcmData(bytes);
    expect(parsed.audioFormat).toBe(1);
    expect(parsed.channels).toBe(1);
    expect(parsed.sampleRate).toBe(16000);
    expect(parsed.bitsPerSample).toBe(16);
    expect([...parsed.data]).toEqual([10, 20, 30, 40]);
  });

  it("handles odd-sized data chunks with padding", async () => {
    // 3-byte data chunk forces the size%2 padding path when walking chunks.
    const pcm = new Uint8Array([1, 2, 3]);
    const bytes = await blobBytes(wavBlobFromPcm(pcm, 24000, 1));
    const parsed = wavPcmData(bytes);
    expect([...parsed.data]).toEqual([1, 2, 3]);
  });

  it("throws on a too-small buffer", () => {
    expect(() => wavPcmData(new Uint8Array(10))).toThrow("WAV chunk is too small.");
  });

  it("throws on a bad RIFF/WAVE header", () => {
    const bad = new Uint8Array(44);
    expect(() => wavPcmData(bad)).toThrow("WAV chunk has invalid RIFF/WAVE header.");
  });
});

describe("concatWavChunksWithBoundarySilence", () => {
  it("stitches two matching WAV chunks with silence between", async () => {
    const wavA = new Uint8Array(
      await wavBlobFromPcm(new Uint8Array([1, 2, 3, 4]), 24000, 1).arrayBuffer(),
    );
    const wavB = new Uint8Array(
      await wavBlobFromPcm(new Uint8Array([5, 6, 7, 8]), 24000, 1).arrayBuffer(),
    );
    const stitched = await blobBytes(concatWavChunksWithBoundarySilence([wavA, wavB]));
    const parsed = wavPcmData(stitched);
    const silence = pcmBoundarySilence(24000, 1).length;
    expect(parsed.data.length).toBe(4 + silence + 4);
    expect([...parsed.data.slice(0, 4)]).toEqual([1, 2, 3, 4]);
    expect([...parsed.data.slice(parsed.data.length - 4)]).toEqual([5, 6, 7, 8]);
  });

  it("throws on mismatched sample rates", async () => {
    const wavA = new Uint8Array(
      await wavBlobFromPcm(new Uint8Array([1, 2]), 24000, 1).arrayBuffer(),
    );
    const wavB = new Uint8Array(
      await wavBlobFromPcm(new Uint8Array([3, 4]), 16000, 1).arrayBuffer(),
    );
    await expect(async () => concatWavChunksWithBoundarySilence([wavA, wavB])).rejects.toThrow(
      "WAV chunks have mismatched sample rates or channels.",
    );
  });

  it("throws when there are no chunks", () => {
    expect(() => concatWavChunksWithBoundarySilence([])).toThrow("No WAV chunks to stitch.");
  });
});
