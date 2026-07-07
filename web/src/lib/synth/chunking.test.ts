import { describe, expect, it } from "vitest";
import {
  TTS_CHUNK_BOUNDARY_SILENCE_MS,
  TTS_CHUNK_MAX_CHARS,
  TTS_CHUNK_MIN_CHARS,
  splitIndexAtOrBefore,
  splitTtsText,
} from "./chunking.ts";

describe("chunking constants", () => {
  it("matches the legacy values", () => {
    expect(TTS_CHUNK_MIN_CHARS).toBe(1600);
    expect(TTS_CHUNK_MAX_CHARS).toBe(900);
    expect(TTS_CHUNK_BOUNDARY_SILENCE_MS).toBe(180);
  });
});

describe("splitIndexAtOrBefore", () => {
  it("prefers sentence boundaries over spaces", () => {
    // '. ' at index 11 -> returns 13.
    expect(splitIndexAtOrBefore("Hello world. Go", 15)).toBe(13);
  });

  it("falls back to the hard limit with no boundary", () => {
    expect(splitIndexAtOrBefore("noboundaryhere", 5)).toBe(5);
  });
});

describe("splitTtsText", () => {
  it("returns short input as a single chunk", () => {
    expect(splitTtsText("hello", 10)).toEqual(["hello"]);
  });

  it("does not split exact-limit input", () => {
    expect(splitTtsText("abcde", 5)).toEqual(["abcde"]);
  });

  it("splits one past the limit at the hard boundary", () => {
    expect(splitTtsText("abcdef", 5)).toEqual(["abcde", "f"]);
  });

  it("splits multi-sentence text at the period boundary", () => {
    expect(splitTtsText("Hello world. Goodbye moon.", 15)).toEqual([
      "Hello world.",
      "Goodbye moon.",
    ]);
  });

  it("honors the boundary priority order", () => {
    expect(splitTtsText("one; two, three four five", 10)).toEqual([
      "one;",
      "two,",
      "three",
      "four five",
    ]);
  });

  it("trims surrounding whitespace", () => {
    expect(splitTtsText("  padded  ", 20)).toEqual(["padded"]);
  });

  it("counts by codepoint, not UTF-16 unit", () => {
    expect(splitTtsText("😀😀😀😀😀😀", 3)).toEqual(["😀😀😀", "😀😀😀"]);
  });

  it("returns [] for empty/whitespace input", () => {
    expect(splitTtsText("   ", 10)).toEqual([]);
    expect(splitTtsText("", 10)).toEqual([]);
  });

  it("defaults maxChars to TTS_CHUNK_MAX_CHARS", () => {
    const short = "x".repeat(TTS_CHUNK_MAX_CHARS);
    expect(splitTtsText(short)).toEqual([short]);
    const long = "x".repeat(TTS_CHUNK_MAX_CHARS + 1);
    expect(splitTtsText(long)).toEqual([short, "x"]);
  });
});
