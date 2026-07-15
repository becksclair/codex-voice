import { describe, expect, it } from "vitest";
import type { EffectiveSpeechPrep } from "./types.ts";
import {
  bracketTags,
  fallbackPerformanceTag,
  fallbackPerformanceTags,
  performanceTagsAreValid,
  performanceTagsPreserveText,
  preservationRatio,
  repairBareLeadingPerformanceCue,
  styleInstructionIsValid,
  textWords,
} from "./tags.ts";

const prep = {
  tagPalette: ["whispers", "softly", "sigh of relief", "tender"],
  maxLength: 60,
  mode: "performance-tags",
} as unknown as EffectiveSpeechPrep;

describe("textWords / bracketTags", () => {
  it("tokenizes ignoring bracket tags", () => {
    expect(textWords("[softly] Hello, world!")).toEqual(["hello", "world"]);
    expect(bracketTags("[softly] hi [warmly] there")).toEqual(["[softly]", "[warmly]"]);
  });

  it("tokenizes non-Latin scripts", () => {
    expect(textWords("Привет мир مرحبا 世界")).toEqual(["привет", "мир", "مرحبا", "世界"]);
  });

  it("keeps combining marks attached to their words", () => {
    expect(textWords("कि कु")).toEqual(["कि", "कु"]);
  });
});

describe("performanceTagsPreserveText", () => {
  it("accepts a tagged version that keeps the words", () => {
    expect(performanceTagsPreserveText("hello there friend", "[softly] hello there friend")).toBe(
      true,
    );
  });

  it("rejects when words are dropped", () => {
    expect(performanceTagsPreserveText("hello there friend", "[softly] hello")).toBe(false);
  });

  it("rejects a complete non-Latin rewrite", () => {
    expect(performanceTagsPreserveText("Привет мир", "[excited] Совсем другой текст")).toBe(false);
  });

  it("rejects rewrites that differ only by combining marks", () => {
    expect(performanceTagsPreserveText("कि", "[softly] कु")).toBe(false);
  });
});

describe("performanceTagsAreValid", () => {
  it("requires added brackets when the text changed", () => {
    expect(performanceTagsAreValid("hello world", "hello world extra")).toBe(false);
  });

  it("accepts unchanged text", () => {
    expect(performanceTagsAreValid("hello world", "hello world")).toBe(true);
  });

  it("accepts bracketed, preserving output", () => {
    expect(performanceTagsAreValid("hello world", "[softly] hello world")).toBe(true);
  });
});

describe("fallbackPerformanceTags", () => {
  it("adds a local tag matched from the palette", () => {
    const out = fallbackPerformanceTags("I finally breathe, safe at last", prep, "inline-tags");
    expect(out).toBe("[sigh of relief] I finally breathe, safe at last");
  });

  it("adds context-local tags for every matching sentence transition", () => {
    const input =
      "I was terrified and could feel the panic rising. Then she smiled, finally safe at last. We laughed and celebrated the victory.";
    const richPrep = {
      ...prep,
      tagPalette: ["fearful", "sigh of relief", "laughs", "proud"],
      maxLength: 500,
    } as unknown as EffectiveSpeechPrep;

    expect(fallbackPerformanceTags(input, richPrep, "inline-tags")).toBe(
      "[fearful] I was terrified and could feel the panic rising. [sigh of relief] Then she smiled, finally safe at last. [laughs] We laughed and celebrated the victory.",
    );
  });

  it("returns null when the tagged result exceeds maxLength", () => {
    const long = "I finally breathe safe at last after a very very long ordeal indeed";
    expect(fallbackPerformanceTags(long, prep, "inline-tags")).toBeNull();
  });

  it("returns null when the input already has tags", () => {
    expect(fallbackPerformanceTags("[whispers] safe at last", prep, "inline-tags")).toBeNull();
  });

  it("returns null for non inline-tags strategies", () => {
    expect(fallbackPerformanceTags("safe at last", prep, "style-instruction")).toBeNull();
  });

  it("returns null when no palette tag matches", () => {
    expect(fallbackPerformanceTag("nothing evocative here", prep)).toBeNull();
    expect(fallbackPerformanceTags("nothing evocative here", prep, "inline-tags")).toBeNull();
  });
});

describe("repairBareLeadingPerformanceCue", () => {
  it("brackets a bare leading cue not present in the source", () => {
    const repaired = repairBareLeadingPerformanceCue(
      "Come here now",
      "softly: Come here now",
      prep,
    );
    expect(repaired).toBe("[softly] Come here now");
  });

  it("leaves identical text untouched", () => {
    expect(repairBareLeadingPerformanceCue("Come here", "Come here", prep)).toBe("Come here");
  });
});

describe("styleInstructionIsValid", () => {
  it("accepts a short directive that does not echo the text", () => {
    expect(
      styleInstructionIsValid(
        "The mission failed and everyone is scared",
        "Speak with quiet dread and slow, careful pacing",
      ),
    ).toBe(true);
  });

  it("rejects bracketed instructions", () => {
    expect(styleInstructionIsValid("x y z", "speak [softly] please")).toBe(false);
  });

  it("rejects overly long instructions", () => {
    expect(styleInstructionIsValid("x y z", "a ".repeat(200))).toBe(false);
  });

  it("rejects labeled preambles", () => {
    expect(styleInstructionIsValid("x y z", "Instruction: speak gently and slowly here")).toBe(
      false,
    );
  });

  it("rejects near-copies of a long input", () => {
    const input = "the quick brown fox jumps over the lazy dog again";
    expect(styleInstructionIsValid(input, input)).toBe(false);
    expect(preservationRatio(input, input)).toBe(1);
  });
});
