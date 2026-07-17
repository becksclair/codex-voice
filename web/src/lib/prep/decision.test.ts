import { describe, expect, it } from "vitest";
import type { BrowserTtsConfig } from "../config.ts";
import {
  browserSpeechPrepForDirect,
  googleSpeechPrepFallback,
  prepareDecision,
  providerMaxTextLength,
  shortenFitLimit,
  speechPrepForProviderLimit,
  speechPrepForStreaming,
  speechPrepStrategy,
  truncateToChars,
} from "./decision.ts";
import type { EffectiveSpeechPrep, PrepSettings } from "./types.ts";

const settingsAllOn: PrepSettings = {
  model: "default",
  emotionPreprocessing: true,
  summarization: true,
};

function tagsPrep(overrides: Partial<EffectiveSpeechPrep> = {}): EffectiveSpeechPrep {
  return {
    provider: "google",
    mode: "performance-tags",
    strategies: { google: "inline-tags", elevenlabs: "inline-tags", default: "inline-tags" },
    tagPalette: ["softly", "warmly"],
    capPerformanceTags: true,
    browserSupported: true,
    baseUrl: "https://gl.example/v1beta",
    apiKey: "k",
    model: "gemini-3.1-flash-tts",
    fallbackModels: [],
    threshold: 10,
    maxInputLength: 5000,
    maxLength: 2000,
    attemptTimeoutMs: 4000,
    timeoutMs: 30000,
    ...overrides,
  };
}

function shortenPrep(overrides: Partial<EffectiveSpeechPrep> = {}): EffectiveSpeechPrep {
  return tagsPrep({ mode: "shorten", maxLength: 100, threshold: 10, ...overrides });
}

describe("prepareDecision", () => {
  it("skips performance-tags when emotion prep is off", () => {
    const d = prepareDecision("x".repeat(50), tagsPrep(), "inline-tags", {
      ...settingsAllOn,
      emotionPreprocessing: false,
    });
    expect(d).toEqual({ shouldPrepare: false, reason: "Emotion prep is off." });
  });

  it("skips shorten when summarization is off and not forced", () => {
    const d = prepareDecision("x".repeat(5000), shortenPrep(), "shorten", {
      ...settingsAllOn,
      summarization: false,
    });
    expect(d.reason).toBe("Summarization is off.");
  });

  it("runs forced shorten even when summarization is off", () => {
    const prep = shortenPrep({ forceSummarization: true });
    const d = prepareDecision("x".repeat(5000), prep, "shorten", {
      ...settingsAllOn,
      summarization: false,
    });
    expect(d.shouldPrepare).toBe(true);
  });

  it("skips below the threshold", () => {
    const d = prepareDecision("short", tagsPrep({ threshold: 100 }), "inline-tags", settingsAllOn);
    expect(d.reason).toBe("Text is below the prep threshold.");
  });

  it("skips when above maxInputLength and not forced", () => {
    const d = prepareDecision(
      "x".repeat(200),
      tagsPrep({ maxInputLength: 100 }),
      "inline-tags",
      settingsAllOn,
    );
    expect(d.reason).toBe("Text is too long for prep.");
  });

  it("skips performance-tags when the strategy resolved off", () => {
    const d = prepareDecision("x".repeat(50), tagsPrep(), "off", settingsAllOn);
    expect(d.reason).toBe("Speech model does not support configured emotion prep.");
  });

  it("skips shorten when the text already fits without summarization", () => {
    // shortenPrepareFloor = max(threshold, min(4000, maxLength)) = min(4000, 5000)=4000
    const d = prepareDecision(
      "x".repeat(3000),
      shortenPrep({ maxLength: 5000, threshold: 10 }),
      "shorten",
      settingsAllOn,
    );
    expect(d.reason).toBe("Text already fits without summarization.");
  });

  it("skips shorten when the text already fits the speech limit", () => {
    // floor = max(threshold, min(4000, maxLength)) = 4000; 4500 clears the floor
    // but still fits the 5000 speech limit.
    const d = prepareDecision(
      "x".repeat(4500),
      shortenPrep({ maxLength: 5000, threshold: 10, maxInputLength: 10000 }),
      "shorten",
      settingsAllOn,
    );
    expect(d.reason).toBe("Text already fits the speech limit.");
  });

  it("prepares performance tags for text above threshold and within limits", () => {
    const d = prepareDecision("x".repeat(50), tagsPrep(), "inline-tags", settingsAllOn);
    expect(d).toEqual({ shouldPrepare: true, reason: "" });
  });
});

describe("speechPrepStrategy", () => {
  function configWith(prep: EffectiveSpeechPrep, inlineAudioTags?: boolean): BrowserTtsConfig {
    return {
      providers: {
        google: {
          model: "gemini-3.1-flash-tts",
          inlineAudioTags,
        },
      },
      speechPrep: prep,
    } as unknown as BrowserTtsConfig;
  }

  it("returns shorten for shorten mode", () => {
    expect(speechPrepStrategy(configWith(shortenPrep()), "google")).toBe("shorten");
  });

  it("returns inline-tags when the provider supports audio tags", () => {
    expect(speechPrepStrategy(configWith(tagsPrep(), true), "google")).toBe("inline-tags");
  });

  it("returns off when audio tags are unsupported", () => {
    expect(speechPrepStrategy(configWith(tagsPrep(), false), "google")).toBe("off");
  });

  it("returns style-instruction for a supported google model", () => {
    const prep = tagsPrep({
      strategies: { google: "style-instruction", elevenlabs: "off", default: "off" },
    });
    expect(speechPrepStrategy(configWith(prep, false), "google")).toBe("style-instruction");
  });
});

describe("config transforms", () => {
  it("swaps server-only prep for a configured google fallback", () => {
    const prep = tagsPrep({
      browserSupported: false,
      browserFallback: {
        provider: "google",
        apiKey: "fk",
        baseUrl: "https://fb.example",
        model: "gemini-2.5",
        fallbackModels: ["gemini-x"],
      },
    });
    const config = { speechPrep: prep } as unknown as BrowserTtsConfig;
    const resolved = browserSpeechPrepForDirect(config);
    expect(resolved?.browserSupported).toBe(true);
    expect(resolved?.provider).toBe("google");
    expect(resolved?.apiKey).toBe("fk");
    expect(resolved?.baseUrl).toBe("https://fb.example");
    expect(resolved?.codexAuth).toBeNull();
  });

  it("passes server-only prep through unchanged with no fallback", () => {
    const prep = tagsPrep({ browserSupported: false });
    const config = { speechPrep: prep } as unknown as BrowserTtsConfig;
    expect(browserSpeechPrepForDirect(config)).toBe(prep);
  });

  it("returns null when there is no fallback for googleSpeechPrepFallback", () => {
    expect(googleSpeechPrepFallback(tagsPrep({ browserSupported: false }))).toBeNull();
  });

  it("forces a shorten prep sized to the provider limit", () => {
    const forced = speechPrepForProviderLimit(tagsPrep(), 1200);
    expect(forced?.mode).toBe("shorten");
    expect(forced?.maxLength).toBe(1200);
    expect(forced?.forceSummarization).toBe(true);
    expect(forced?.threshold).toBe(Math.min(1200, 4000));
  });

  it("drops the streaming threshold to 0 only for performance-tags", () => {
    expect(speechPrepForStreaming(tagsPrep())?.threshold).toBe(0);
    const shorten = shortenPrep({ threshold: 42 });
    expect(speechPrepForStreaming(shorten)?.threshold).toBe(42);
  });

  it("caps the shorten fit limit at MIN_SHORTEN_OUTPUT_CHARS", () => {
    expect(shortenFitLimit(1000)).toBe(1000);
    expect(shortenFitLimit(10000)).toBe(4000);
  });

  it("truncates by codepoints", () => {
    expect(truncateToChars("hello", 3)).toBe("hel");
    expect(truncateToChars("hi", 10)).toBe("hi");
    expect(truncateToChars("hi", Infinity)).toBe("hi");
  });
});

describe("providerMaxTextLength", () => {
  it("prefers the provider max, then config max, then Infinity", () => {
    const config = {
      maxTextLength: 5000,
      providers: { google: { maxTextLength: 1234 } },
    } as unknown as BrowserTtsConfig;
    expect(providerMaxTextLength(config, "google")).toBe(1234);
    expect(providerMaxTextLength(config, "elevenlabs")).toBe(5000);
    expect(providerMaxTextLength(null, "google")).toBe(Infinity);
  });

  it("caps ElevenLabs v3 browser requests at the upstream 5000-character limit", () => {
    const config = {
      maxTextLength: 6000,
      providers: { elevenlabs: { modelId: "eleven_v3", maxTextLength: 6000 } },
    } as unknown as BrowserTtsConfig;
    expect(providerMaxTextLength(config, "elevenlabs", "default")).toBe(5000);
    expect(providerMaxTextLength(config, "elevenlabs", "elevenlabs:eleven_flash_v2_5")).toBe(6000);
    config.providers.elevenlabs!.maxTextLength = 5000;
    expect(providerMaxTextLength(config, "elevenlabs", "elevenlabs:eleven_flash_v2_5")).toBe(5000);
    config.providers.elevenlabs!.maxTextLengthOverridden = false;
    expect(providerMaxTextLength(config, "elevenlabs", "elevenlabs:eleven_flash_v2_5")).toBe(6000);
    config.providers.elevenlabs!.maxTextLengthOverridden = true;
    expect(providerMaxTextLength(config, "elevenlabs", "default")).toBe(5000);
  });
});
