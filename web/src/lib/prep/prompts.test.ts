import { describe, expect, it } from "vitest";
import type { EffectiveSpeechPrep } from "./types.ts";
import { buildShortenPrompt } from "./prompts.ts";

describe("buildShortenPrompt", () => {
  it("requires faithful literary compression within the hard length contract", () => {
    const prompt = buildShortenPrompt("A vivid source passage.", {
      maxLength: 4000,
    } as EffectiveSpeechPrep);

    expect(prompt).toContain("no more than 4000 characters");
    expect(prompt).toContain("at least 23 characters");
    expect(prompt).toContain("complete semantic meaning");
    expect(prompt).toContain("author's voice and point of view");
    expect(prompt).toContain("distinctive imagery");
    expect(prompt).toContain("Do not sanitize, moralize, euphemize");
    expect(prompt).toContain("make surgical cuts");
    expect(prompt).toContain("Do not add bracketed performance tags");
    expect(prompt).toContain('Text:\n"""A vivid source passage."""');
  });
});
