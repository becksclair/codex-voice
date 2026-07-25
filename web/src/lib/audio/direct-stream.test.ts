import { describe, expect, test } from "vitest";
import type { BrowserTtsConfig } from "../config.ts";
import { canStreamElevenLabsDirect, directStreamTimeoutMs } from "./direct-stream.ts";

const config = {
  providers: {
    elevenlabs: {
      models: ["eleven_v3", "eleven_flash_v2_5"],
    },
  },
} as BrowserTtsConfig;

describe("direct stream capability", () => {
  test("limits ElevenLabs direct streaming to the v3 model family", () => {
    expect(canStreamElevenLabsDirect(config, "elevenlabs:eleven_v3")).toBe(true);
    expect(canStreamElevenLabsDirect(config, "elevenlabs:eleven_v3_alpha")).toBe(true);
    expect(canStreamElevenLabsDirect(config, "elevenlabs:eleven_flash_v2_5")).toBe(false);
  });
});

describe("direct stream timeout", () => {
  test("preserves the configured timeout for short input", () => {
    expect(directStreamTimeoutMs(30_000, "a".repeat(1_200))).toBe(30_000);
  });

  test("scales and caps long-input timeouts like the Rust provider path", () => {
    expect(directStreamTimeoutMs(30_000, "a".repeat(4_000))).toBe(160_000);
    expect(directStreamTimeoutMs(30_000, "a".repeat(20_000))).toBe(300_000);
  });
});
