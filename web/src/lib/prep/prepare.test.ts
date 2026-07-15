import { afterEach, describe, expect, it, vi } from "vitest";
import type { BrowserTtsConfig } from "../config.ts";
import { prepareForProvider } from "./prepare.ts";
import type { EffectiveSpeechPrep, PrepSettings } from "./types.ts";

const settings: PrepSettings = {
  model: "default",
  emotionPreprocessing: true,
  summarization: false,
};

function googleTextResponse(text: string, status = 200): Response {
  return new Response(JSON.stringify({ candidates: [{ content: { parts: [{ text }] } }] }), {
    status,
  });
}

function baseConfig(prep: Partial<EffectiveSpeechPrep>, googleMax = 100000): BrowserTtsConfig {
  return {
    version: 1,
    defaultProvider: "google",
    maxTextLength: 100000,
    providers: {
      google: {
        model: "gemini-3.1-flash-tts",
        inlineAudioTags: true,
        baseUrl: "https://gl.example/v1beta",
        apiKey: "gk",
        voice: "Kore",
        maxTextLength: googleMax,
      },
    },
    personas: {},
    speechPrep: {
      provider: "google",
      mode: "performance-tags",
      strategies: { google: "inline-tags", elevenlabs: "inline-tags", default: "inline-tags" },
      tagPalette: ["softly", "sigh of relief"],
      capPerformanceTags: true,
      browserSupported: true,
      baseUrl: "https://gl.example/v1beta",
      apiKey: "gk",
      model: "gemini-3.1-flash-tts",
      fallbackModels: [],
      threshold: 5,
      maxInputLength: 100000,
      maxLength: 1000,
      attemptTimeoutMs: 4000,
      timeoutMs: 30000,
      ...prep,
    },
  } as unknown as BrowserTtsConfig;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("prepareForProvider — browser-supported inline tags", () => {
  it("returns the tagged text on success", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleTextResponse("[softly] Hello world")),
    );
    const result = await prepareForProvider(
      baseConfig({}),
      "google",
      "Hello world",
      null,
      settings,
    );
    expect(result.input).toBe("[softly] Hello world");
    expect(result.changed).toBe(true);
    expect(result.strategy).toBe("inline-tags");
    expect(result.error).toBeUndefined();
  });

  it("skips below the threshold without calling fetch", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const result = await prepareForProvider(
      baseConfig({ threshold: 50 }),
      "google",
      "hi",
      null,
      settings,
    );
    expect(result.skipped).toBe(true);
    expect(result.reason).toBe("Text is below the prep threshold.");
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("prepareForProvider — refreshed Codex auth", () => {
  it("reports rotated credentials from a transformed streaming prep clone", async () => {
    const expired = `header.${btoa(JSON.stringify({ exp: 1 }))}.signature`;
    const fresh = `header.${btoa(
      JSON.stringify({ exp: Math.floor(Date.now() / 1000) + 3600 }),
    )}.signature`;
    const config = baseConfig({
      provider: "codex",
      baseUrl: "/_codex",
      model: "gpt-test",
      codexAuth: {
        accessToken: expired,
        refreshToken: "initial-refresh",
        accountId: "account-id",
        tokenUrl: "https://auth.example.test/oauth/token",
        clientId: "client-id",
      },
    });
    const onCodexAuthRefreshed = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("auth.example.test")) {
          return new Response(
            JSON.stringify({
              access_token: fresh,
              refresh_token: "rotated-refresh",
              account_id: "account-id",
            }),
          );
        }
        if (url === "/_codex/responses") {
          return new Response(
            'data: {"type":"response.output_text.delta","delta":"[softly] Hello world"}\n\n' +
              "data: [DONE]\n\n",
          );
        }
        throw new Error(`unexpected fetch ${url}`);
      }),
    );

    await prepareForProvider(config, "google", "Hello world", null, settings, {
      forcePerformanceTags: true,
      onCodexAuthRefreshed,
    });

    expect(onCodexAuthRefreshed).toHaveBeenCalledTimes(1);
    const refreshed = onCodexAuthRefreshed.mock.calls[0][0] as EffectiveSpeechPrep;
    expect(refreshed).not.toBe(config.speechPrep);
    expect(refreshed.codexAuth?.refreshToken).toBe("rotated-refresh");
  });
});

describe("prepareForProvider — server-only prep", () => {
  it("throws a non-retryable error when browser prep is required", async () => {
    const config = baseConfig({ browserSupported: false });
    await expect(
      prepareForProvider(config, "google", "Hello world", null, settings, {
        requireBrowserPrep: true,
      }),
    ).rejects.toThrow("Configured emotion prep is server-only.");
  });

  it("returns a skipped result when browser prep is not required", async () => {
    const config = baseConfig({ browserSupported: false });
    const result = await prepareForProvider(config, "google", "Hello world", null, settings);
    expect(result.skipped).toBe(true);
    expect(result.error).toBe("Configured emotion prep is server-only.");
    expect(result.input).toBe("Hello world");
  });
});

describe("prepareForProvider — failure handling", () => {
  it("uses context-local tags on a non-retryable provider error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleTextResponse("nope", 400)),
    );
    const result = await prepareForProvider(
      baseConfig({ tagPalette: ["fearful", "sigh of relief", "laughs"] }),
      "google",
      "I was terrified. Then I was safe at last. We laughed.",
      null,
      settings,
    );
    expect(result.input).toBe(
      "[fearful] I was terrified. [sigh of relief] Then I was safe at last. [laughs] We laughed.",
    );
    expect(result.changed).toBe(true);
    expect(result.warning).toContain("Emotion prep failed");
  });

  it("prefers broader local transition coverage over an unchanged remote result", async () => {
    const input = "I was terrified. Then I was safe at last. We laughed.";
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleTextResponse(input)),
    );

    const result = await prepareForProvider(
      baseConfig({ tagPalette: ["fearful", "sigh of relief", "laughs"] }),
      "google",
      input,
      null,
      settings,
    );

    expect(result.input).toBe(
      "[fearful] I was terrified. [sigh of relief] Then I was safe at last. [laughs] We laughed.",
    );
    expect(result.warning).toContain("coverage");
  });

  it("keeps a successful model result once it has more than two tags", async () => {
    const input = "I was terrified. Then I was safe at last. We laughed. I was terrified again.";
    const remote =
      "[fearful] I was terrified. [sigh of relief] Then I was safe at last. [laughs] We laughed. I was terrified again.";
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleTextResponse(remote)),
    );

    const result = await prepareForProvider(
      baseConfig({ tagPalette: ["fearful", "sigh of relief", "laughs"] }),
      "google",
      input,
      null,
      settings,
    );

    expect(result.input).toBe(remote);
    expect(result.warning).toBeUndefined();
  });

  it("uses a local sparse tag when the model retries out", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("boom", { status: 500 })),
    );
    const result = await prepareForProvider(
      baseConfig({ tagPalette: ["sigh of relief"] }),
      "google",
      "I finally breathe, safe at last",
      null,
      settings,
    );
    expect(result.input).toBe("[sigh of relief] I finally breathe, safe at last");
    expect(result.changed).toBe(true);
    expect(result.warning).toBeTruthy();
  });
});

describe("prepareForProvider — forced shorten", () => {
  it("falls back to a fitted excerpt when the summary is too short", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleTextResponse("short")),
    );
    const config = baseConfig({}, 10);
    const input = "abcdefghijklmnopqrst"; // 20 chars > provider max 10
    const result = await prepareForProvider(config, "google", input, null, settings);
    expect(result.strategy).toBe("shorten");
    expect(result.input).toBe(input.slice(0, 10));
    expect(result.warning).toContain("below the minimum length");
  });
});

describe("prepareForProvider — cache", () => {
  it("reuses a cached result for the same key", async () => {
    const fetchMock = vi.fn(async () => googleTextResponse("[softly] Hello world"));
    vi.stubGlobal("fetch", fetchMock);
    const cache = new Map();
    const config = baseConfig({});
    await prepareForProvider(config, "google", "Hello world", null, settings, { prepCache: cache });
    await prepareForProvider(config, "google", "Hello world", null, settings, { prepCache: cache });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
