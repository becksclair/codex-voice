import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowserTtsConfig } from "./config.ts";
import {
  fetchConfig,
  loadCachedConfig,
  reconcileBrowserConfig,
  saveCachedConfig,
  syncCodexAuthToServer,
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
      },
      elevenlabs: {
        apiKey: "el-key",
        baseUrl: "https://api.elevenlabs.io",
        modelId: "eleven_v3",
        fallbackModels: [],
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

function jwt(exp: number): string {
  return `header.${btoa(JSON.stringify({ exp }))}.signature`;
}

function withCodexAuth(
  config: BrowserTtsConfig,
  auth: { accessToken: string; refreshToken: string; accountId: string },
): BrowserTtsConfig {
  config.speechPrep = {
    provider: "codex",
    mode: "performance-tags",
    strategies: { google: "style", elevenlabs: "tags", default: "tags" },
    tagPalette: ["[laughs]"],
    capPerformanceTags: true,
    browserSupported: true,
    baseUrl: "/_codex",
    model: "gpt",
    fallbackModels: [],
    threshold: 100,
    maxInputLength: 4000,
    maxLength: 4000,
    attemptTimeoutMs: 4000,
    timeoutMs: 30000,
    codexAuth: auth,
  };
  return config;
}

beforeEach(() => {
  localStorage.clear();
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("reconcileBrowserConfig", () => {
  it("preserves a newer cached token bundle for the same account", () => {
    const fresh = withCodexAuth(fixture(), {
      accessToken: jwt(100),
      refreshToken: "server-refresh",
      accountId: "account-1",
    });
    const cached = withCodexAuth(fixture(), {
      accessToken: jwt(200),
      refreshToken: "browser-refresh",
      accountId: "account-1",
    });

    const result = reconcileBrowserConfig(fresh, cached);
    expect(result.speechPrep?.codexAuth?.refreshToken).toBe("browser-refresh");
  });

  it("uses a newer server bundle and replaces a different account", () => {
    const cached = withCodexAuth(fixture(), {
      accessToken: jwt(100),
      refreshToken: "cached-refresh",
      accountId: "account-1",
    });
    const newer = withCodexAuth(fixture(), {
      accessToken: jwt(200),
      refreshToken: "server-refresh",
      accountId: "account-1",
    });
    expect(reconcileBrowserConfig(newer, cached).speechPrep?.codexAuth?.refreshToken).toBe(
      "server-refresh",
    );

    const otherAccount = withCodexAuth(fixture(), {
      accessToken: jwt(50),
      refreshToken: "other-refresh",
      accountId: "account-2",
    });
    expect(reconcileBrowserConfig(otherAccount, cached).speechPrep?.codexAuth?.accountId).toBe(
      "account-2",
    );
  });

  it("treats a fresh config without complete auth as authoritative", () => {
    const fresh = fixture();
    fresh.speechPrep = withCodexAuth(fixture(), {
      accessToken: jwt(200),
      refreshToken: "server-refresh",
      accountId: "account-1",
    }).speechPrep;
    delete fresh.speechPrep?.codexAuth;
    fresh.speechPrep!.browserSupported = false;
    const cached = withCodexAuth(fixture(), {
      accessToken: jwt(100),
      refreshToken: "cached-refresh",
      accountId: "account-1",
    });

    expect(reconcileBrowserConfig(fresh, cached).speechPrep?.codexAuth).toBeUndefined();
  });
});

describe("loadCachedConfig / saveCachedConfig", () => {
  it("round-trips through localStorage", () => {
    const config = withCodexAuth(fixture(), {
      accessToken: "access",
      refreshToken: "refresh",
      accountId: "account",
    });
    saveCachedConfig(config);
    expect(loadCachedConfig()?.speechPrep?.codexAuth?.refreshToken).toBe("refresh");
  });

  it("returns null on absence", () => {
    expect(loadCachedConfig()).toBeNull();
  });

  it("returns null and does not throw on corrupt JSON", () => {
    localStorage.setItem(CONFIG_STORAGE_KEY, "{not json");
    expect(loadCachedConfig()).toBeNull();
  });
});

describe("syncCodexAuthToServer", () => {
  it("posts a pending complete bundle and clears the pending marker", async () => {
    const config = withCodexAuth(fixture(), {
      accessToken: jwt(200),
      refreshToken: "rotated-refresh",
      accountId: "account-1",
    });
    config.speechPrep!.codexAuth!.serverSyncPending = true;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(null, { status: 204 })),
    );

    expect(await syncCodexAuthToServer(config)).toBe(true);
    expect(fetch).toHaveBeenCalledWith(
      "/web/codex-auth",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          accessToken: jwt(200),
          refreshToken: "rotated-refresh",
          accountId: "account-1",
        }),
      }),
    );
    expect(config.speechPrep?.codexAuth?.serverSyncPending).toBe(false);
    expect(loadCachedConfig()?.speechPrep?.codexAuth?.serverSyncPending).toBe(false);
  });

  it("keeps the marker pending when the server is unavailable", async () => {
    const config = withCodexAuth(fixture(), {
      accessToken: jwt(200),
      refreshToken: "rotated-refresh",
      accountId: "account-1",
    });
    config.speechPrep!.codexAuth!.serverSyncPending = true;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(null, { status: 502 })),
    );

    expect(await syncCodexAuthToServer(config)).toBe(false);
    expect(config.speechPrep?.codexAuth?.serverSyncPending).toBe(true);
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
