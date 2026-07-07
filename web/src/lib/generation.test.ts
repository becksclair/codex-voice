import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowserPersonaConfig, BrowserTtsConfig } from "./config.ts";
import {
  canGenerateDirectWithConfiguredPrep,
  fallbackProvider,
  GenerationController,
  isRetryable,
  resolveProvider,
  settingsMatchServerDefaults,
} from "./generation.ts";
import type { WebSettings } from "./settings.ts";
import { DEFAULT_SETTINGS } from "./settings.ts";
import {
  GENERATION_STATE_STORAGE_KEY,
  getLastGeneratedAudio,
  loadPendingGeneration,
} from "./storage.ts";

/** Route a mock fetch by URL substring. */
function routedFetch(routes: { match: string; respond: () => Response }[]): typeof fetch {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const route = routes.find((r) => url.includes(r.match));
    if (!route) throw new Error(`unrouted fetch: ${url}`);
    return route.respond();
  }) as unknown as typeof fetch;
}

const noPrep = {
  provider: "google",
  mode: "performance-tags",
  strategies: { google: "inline-tags", elevenlabs: "inline-tags", default: "inline-tags" },
  tagPalette: [],
  capPerformanceTags: true,
  browserSupported: true,
  baseUrl: "https://gl.example/v1beta",
  apiKey: "gk",
  model: "gemini-3.1-flash-tts",
  fallbackModels: [],
  threshold: 1_000_000, // above any test input, so prep always skips (no prep fetch)
  maxInputLength: 1_000_000,
  maxLength: 1000,
  attemptTimeoutMs: 4000,
  timeoutMs: 30000,
};

function directConfig(): BrowserTtsConfig {
  return {
    version: 1,
    defaultProvider: "google",
    defaultPersona: "narrator",
    maxTextLength: 1_000_000,
    providers: {
      google: {
        model: "gemini-2.5-flash-tts",
        baseUrl: "https://gl.example/v1beta",
        apiKey: "gk",
        voice: "Kore",
        maxTextLength: 1_000_000,
      },
      elevenlabs: {
        modelId: "eleven_flash_v2",
        baseUrl: "https://api.elevenlabs.io",
        apiKey: "xi",
        outputFormat: "mp3_44100",
        applyTextNormalization: "off",
        streamGain: 1,
      },
    },
    personas: {
      narrator: {
        label: "Narrator",
        description: "",
        provider: "google",
        fallbackPolicy: "preserve-persona",
        promptConstraints: [],
        google: { voiceName: "Charon" },
        elevenlabs: { voiceId: "voice-1", voiceSettings: { speed: 1.0 } },
      } as unknown as BrowserPersonaConfig,
    },
    speechPrep: noPrep,
  } as unknown as BrowserTtsConfig;
}

function googleAudioResponse(): Response {
  const pcmB64 = btoa(String.fromCharCode(0, 64, 0, 128));
  return new Response(
    JSON.stringify({
      candidates: [
        {
          content: {
            parts: [{ inlineData: { data: pcmB64, mimeType: "audio/L16;codec=pcm;rate=24000" } }],
          },
        },
      ],
    }),
    { status: 200 },
  );
}

const settings: WebSettings = { ...DEFAULT_SETTINGS };

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("pure decision helpers", () => {
  it("isRetryable honors the retryable flag and status", () => {
    expect(isRetryable({ retryable: false } as Error & { retryable: boolean })).toBe(false);
    expect(isRetryable(new Error("network"))).toBe(true);
    expect(isRetryable({ status: 500 } as Error & { status: number })).toBe(true);
    expect(isRetryable({ status: 404 } as Error & { status: number })).toBe(false);
  });

  it("fallbackProvider flips provider", () => {
    expect(fallbackProvider("google")).toBe("elevenlabs");
    expect(fallbackProvider("elevenlabs")).toBe("google");
  });

  it("settingsMatchServerDefaults gates the server fallback", () => {
    expect(settingsMatchServerDefaults(DEFAULT_SETTINGS)).toBe(true);
    expect(settingsMatchServerDefaults({ ...DEFAULT_SETTINGS, provider: "google" })).toBe(false);
  });

  it("resolveProvider honors explicit settings then persona", () => {
    const config = directConfig();
    expect(resolveProvider(config, config.personas.narrator, DEFAULT_SETTINGS)).toBe("google");
    expect(resolveProvider(config, null, { ...DEFAULT_SETTINGS, provider: "elevenlabs" })).toBe(
      "elevenlabs",
    );
  });

  it("canGenerateDirectWithConfiguredPrep needs a provider", () => {
    expect(canGenerateDirectWithConfiguredPrep(directConfig())).toBe(true);
    expect(canGenerateDirectWithConfiguredPrep(null)).toBe(false);
  });
});

describe("GenerationController — path selection", () => {
  it("chunked direct path: non-streamable google synthesizes via generateContent", async () => {
    const fetchMock = routedFetch([{ match: ":generateContent", respond: googleAudioResponse }]);
    vi.stubGlobal("fetch", fetchMock);
    const audio: { blob: Blob; provider: string; streamed?: boolean }[] = [];
    const controller = new GenerationController({
      config: directConfig(),
      settings,
      callbacks: {
        onAudioReady: (blob, meta) =>
          audio.push({ blob, provider: meta.provider, streamed: meta.streamed }),
      },
    });
    await controller.generate("Hello there");
    expect(audio).toHaveLength(1);
    expect(audio[0].provider).toBe("google");
    expect(audio[0].streamed).toBeFalsy();
  });

  it("server path: no direct config goes straight to the server job", async () => {
    const wavB64 = btoa("RIFF0000WAVEfmt ");
    const fetchMock = routedFetch([
      {
        match: "/web/speech-jobs/",
        respond: () =>
          new Response(
            JSON.stringify({
              id: "job1",
              status: "complete",
              result: {
                input: "Hello",
                input_changed: false,
                audio_base64: wavB64,
                mime_type: "audio/wav",
                format: "wav",
              },
            }),
            { status: 200 },
          ),
      },
      {
        match: "/web/speech-jobs",
        respond: () => new Response(JSON.stringify({ id: "job1" }), { status: 200 }),
      },
    ]);
    vi.stubGlobal("fetch", fetchMock);
    const audio: string[] = [];
    const controller = new GenerationController({
      config: null,
      settings,
      callbacks: { onAudioReady: (_blob, meta) => audio.push(meta.provider) },
    });
    await controller.generate("Hello");
    expect(audio).toEqual(["server"]);
    // Pending cleared and last audio persisted.
    expect(loadPendingGeneration()).toBeNull();
    expect(await getLastGeneratedAudio()).not.toBeNull();
  });

  it("server fallback: direct failure with default settings falls back to server", async () => {
    const wavB64 = btoa("RIFF0000WAVEfmt ");
    const fetchMock = routedFetch([
      { match: ":generateContent", respond: () => new Response("boom", { status: 500 }) },
      {
        match: "/web/speech-jobs/",
        respond: () =>
          new Response(
            JSON.stringify({
              status: "complete",
              result: {
                input: "Hello",
                input_changed: true,
                audio_base64: wavB64,
                mime_type: "audio/wav",
                format: "wav",
              },
            }),
            { status: 200 },
          ),
      },
      {
        match: "/web/speech-jobs",
        respond: () => new Response(JSON.stringify({ id: "job9" }), { status: 200 }),
      },
    ]);
    vi.stubGlobal("fetch", fetchMock);
    // Google-only config so the retryable failure cannot fall back to elevenlabs.
    const config = directConfig();
    delete (config.providers as { elevenlabs?: unknown }).elevenlabs;
    const audio: string[] = [];
    const controller = new GenerationController({
      config,
      settings,
      callbacks: { onAudioReady: (_blob, meta) => audio.push(meta.provider) },
    });
    await controller.generate("Hello");
    expect(audio).toEqual(["server"]);
  });
});

describe("GenerationController — provider fallback ordering", () => {
  it("falls back to elevenlabs when google fails retryably under preserve-persona", async () => {
    const fetchMock = routedFetch([
      { match: ":generateContent", respond: () => new Response("boom", { status: 500 }) },
      {
        match: "/v1/text-to-speech/",
        respond: () =>
          new Response(new Uint8Array([1, 2, 3, 4]), {
            status: 200,
            headers: { "content-type": "audio/mpeg" },
          }),
      },
    ]);
    vi.stubGlobal("fetch", fetchMock);
    const providers: string[] = [];
    const controller = new GenerationController({
      config: directConfig(),
      settings: { ...settings, voice: "persona:narrator" },
      callbacks: { onAudioReady: (_blob, meta) => providers.push(meta.provider) },
    });
    await controller.generate("Hello there");
    expect(providers).toEqual(["elevenlabs"]);
  });
});

describe("GenerationController — cancellation", () => {
  it("aborts the in-flight fetch and makes no late state writes", async () => {
    const abortErrors: string[] = [];
    const captured: { signal: AbortSignal | null } = { signal: null };
    const fetchMock = vi.fn(
      (_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise((_resolve, reject) => {
          captured.signal = init?.signal ?? null;
          init?.signal?.addEventListener("abort", () => {
            abortErrors.push("aborted");
            reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
          });
        }),
    ) as unknown as typeof fetch;
    vi.stubGlobal("fetch", fetchMock);
    const events: string[] = [];
    const controller = new GenerationController({
      config: null,
      settings,
      callbacks: {
        onAudioReady: () => events.push("audio"),
        onError: () => events.push("error"),
        onTextReplace: () => events.push("text"),
      },
    });
    const promise = controller.generate("Hello");
    await Promise.resolve();
    controller.cancel();
    await promise;
    expect(captured.signal?.aborted).toBe(true);
    expect(abortErrors).toEqual(["aborted"]);
    // Cancelled run must not surface audio, error, or a text replacement.
    expect(events).toEqual([]);
  });
});

describe("GenerationController — persistence points", () => {
  it("writes then clears pending generation across a server run", async () => {
    const wavB64 = btoa("RIFF0000WAVEfmt ");
    let sawPendingDuringPoll = false;
    const fetchMock = routedFetch([
      {
        match: "/web/speech-jobs/",
        respond: () => {
          // By poll time, the job id must be persisted.
          sawPendingDuringPoll = Boolean(localStorage.getItem(GENERATION_STATE_STORAGE_KEY));
          return new Response(
            JSON.stringify({
              status: "complete",
              result: {
                input: "Hi",
                input_changed: false,
                audio_base64: wavB64,
                mime_type: "audio/wav",
                format: "wav",
              },
            }),
            { status: 200 },
          );
        },
      },
      {
        match: "/web/speech-jobs",
        respond: () => new Response(JSON.stringify({ id: "job5" }), { status: 200 }),
      },
    ]);
    vi.stubGlobal("fetch", fetchMock);
    const controller = new GenerationController({ config: null, settings });
    await controller.generate("Hi");
    expect(sawPendingDuringPoll).toBe(true);
    expect(loadPendingGeneration()).toBeNull();
  });
});
