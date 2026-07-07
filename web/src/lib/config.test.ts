import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowserTtsConfig } from "./config.ts";
import {
  fetchConfig,
  loadCachedConfig,
  sanitizeBrowserConfig,
  saveCachedConfig,
} from "./config.ts";
import { CONFIG_STORAGE_KEY } from "./storage.ts";

/** A representative payload mirroring the web.rs serde shape (camelCase). */
function fixture(): BrowserTtsConfig {
  return {
    version: 1,
    defaultProvider: "google",
    defaultPersona: "narrator",
    maxTextLength: 5000,
    providers: {
      google: {
        apiKey: "g-key",
        baseUrl: "https://generativelanguage.googleapis.com/v1beta",
        voice: "Kore",
        model: "gemini-2.5-flash-tts",
        fallbackModels: ["gemini-2.5-pro-tts"],
        streaming: {
          transport: "interactions-stream",
          supportedModels: ["gemini-3.1-flash-tts-preview"],
          outputFormat: "pcm_24000",
          sampleRate: 24000,
          channels: 1,
        },
        maxTextLength: 5000,
        timeoutMs: 30000,
        constraints: [],
      },
      elevenlabs: {
        apiKey: "el-key",
        baseUrl: "https://api.elevenlabs.io",
        modelId: "eleven_v3",
        streaming: {
          transport: "websocket",
          preferredModel: "eleven_flash_v2_5",
          outputFormat: "pcm_24000",
          sampleRate: 24000,
          channels: 1,
          chunkLengthSchedule: [120, 160, 250, 290],
        },
        applyTextNormalization: "auto",
        outputFormat: "mp3_44100_128",
        streamGain: 2,
        maxTextLength: 5000,
        timeoutMs: 30000,
      },
    },
    personas: {
      narrator: {
        label: "Narrator",
        description: "calm",
        provider: "google",
        fallbackPolicy: "preserve-persona",
        promptConstraints: [],
        google: { voiceName: "Kore" },
      },
    },
  };
}

beforeEach(() => {
  localStorage.clear();
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("sanitizeBrowserConfig", () => {
  it("strips speechPrep.codexAuth and nothing else", () => {
    const config = fixture() as BrowserTtsConfig;
    config.speechPrep = {
      provider: "codex",
      mode: "performance-tags",
      strategies: { google: "style", elevenlabs: "tags", default: "tags" },
      tagPalette: ["[laughs]"],
      capPerformanceTags: true,
      browserSupported: true,
      baseUrl: "https://x",
      model: "gpt",
      fallbackModels: [],
      threshold: 100,
      maxInputLength: 4000,
      maxLength: 4000,
      attemptTimeoutMs: 4000,
      timeoutMs: 30000,
      codexAuth: { accessToken: "secret" },
    };
    const result = sanitizeBrowserConfig(config);
    expect(result.speechPrep?.codexAuth).toBeUndefined();
    // Everything else intact.
    expect(result.speechPrep?.model).toBe("gpt");
    expect(result.providers.google?.apiKey).toBe("g-key");
    expect(result.defaultProvider).toBe("google");
  });

  it("is a no-op when codexAuth is absent", () => {
    const config = fixture();
    expect(sanitizeBrowserConfig(config)).toBe(config);
  });
});

describe("loadCachedConfig / saveCachedConfig", () => {
  it("round-trips through localStorage", () => {
    saveCachedConfig(fixture());
    expect(loadCachedConfig()?.defaultProvider).toBe("google");
  });

  it("returns null on absence", () => {
    expect(loadCachedConfig()).toBeNull();
  });

  it("returns null and does not throw on corrupt JSON", () => {
    localStorage.setItem(CONFIG_STORAGE_KEY, "{not json");
    expect(loadCachedConfig()).toBeNull();
  });

  it("re-persists a sanitized cached config", () => {
    const config = fixture() as BrowserTtsConfig;
    config.speechPrep = {
      provider: "codex",
      mode: "m",
      strategies: { google: "", elevenlabs: "", default: "" },
      tagPalette: [],
      capPerformanceTags: false,
      browserSupported: false,
      baseUrl: "",
      model: "",
      fallbackModels: [],
      threshold: 0,
      maxInputLength: 0,
      maxLength: 0,
      attemptTimeoutMs: 0,
      timeoutMs: 0,
      codexAuth: { accessToken: "leak" },
    };
    localStorage.setItem(CONFIG_STORAGE_KEY, JSON.stringify(config));
    loadCachedConfig();
    const persisted = JSON.parse(
      localStorage.getItem(CONFIG_STORAGE_KEY) || "{}",
    ) as BrowserTtsConfig;
    expect(persisted.speechPrep?.codexAuth).toBeUndefined();
  });
});

describe("fetchConfig", () => {
  it("parses a valid /web/config payload", async () => {
    const config = fixture();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify(config), { status: 200 })),
    );
    const result = await fetchConfig();
    expect(result?.version).toBe(1);
    expect(result?.providers.elevenlabs?.modelId).toBe("eleven_v3");
    expect(fetch).toHaveBeenCalledWith("/web/config", { cache: "no-store" });
  });

  it("returns null on a non-OK response", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("nope", { status: 500 })),
    );
    expect(await fetchConfig()).toBeNull();
  });

  it("returns null when version is not 1", async () => {
    const config = { ...fixture(), version: 2 };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify(config), { status: 200 })),
    );
    expect(await fetchConfig()).toBeNull();
  });

  it("returns null when providers is missing", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ version: 1 }), { status: 200 })),
    );
    expect(await fetchConfig()).toBeNull();
  });

  it("returns null on a network error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("offline");
      }),
    );
    expect(await fetchConfig()).toBeNull();
  });
});
