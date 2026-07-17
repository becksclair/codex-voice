import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowserPersonaConfig, BrowserTtsConfig } from "./config.ts";
import type { AudioContextConstructor, StreamingPlayback } from "./audio/streaming.ts";
import {
  canGenerateDirectWithConfiguredPrep,
  canStreamSelectedProvider,
  GenerationController,
  isBackendUnavailable,
  isRetryable,
  serverJobOptions,
} from "./generation.ts";
import { resolveProvider } from "./personas.ts";
import type { WebSettings } from "./settings.ts";
import { DEFAULT_SETTINGS } from "./settings.ts";
import {
  GENERATION_STATE_STORAGE_KEY,
  getLastGeneratedAudio,
  loadPendingGeneration,
  savePendingGeneration,
} from "./storage.ts";

class FakeAudioBuffer {
  duration: number;
  private data: Float32Array[];
  constructor(
    public numberOfChannels: number,
    public length: number,
    public sampleRate: number,
  ) {
    this.duration = length / sampleRate;
    this.data = Array.from({ length: numberOfChannels }, () => new Float32Array(length));
  }
  getChannelData(channel: number): Float32Array {
    return this.data[channel];
  }
}

class FakeBufferSource {
  buffer: FakeAudioBuffer | null = null;
  onended: (() => void) | null = null;
  connect(): void {}
  start(): void {}
}

class FakeAudioContext {
  currentTime = 0;
  state: "running" | "suspended" | "closed" = "running";
  destination = {};
  createBuffer(channels: number, length: number, sampleRate: number): FakeAudioBuffer {
    return new FakeAudioBuffer(channels, length, sampleRate);
  }
  createBufferSource(): FakeBufferSource {
    return new FakeBufferSource();
  }
  async resume(): Promise<void> {
    this.state = "running";
  }
  async suspend(): Promise<void> {
    this.state = "suspended";
  }
  async close(): Promise<void> {
    this.state = "closed";
  }
}

const fakeAudioContextCtor = (): AudioContextConstructor =>
  FakeAudioContext as unknown as AudioContextConstructor;

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

  it("classifies network and gateway failures as backend unavailability", () => {
    expect(isBackendUnavailable(new TypeError("Failed to fetch"))).toBe(true);
    for (const status of [502, 503, 504]) {
      expect(isBackendUnavailable({ status } as Error & { status: number })).toBe(true);
    }
    expect(isBackendUnavailable({ status: 500 } as Error & { status: number })).toBe(false);
    expect(isBackendUnavailable({ name: "AbortError" } as Error)).toBe(false);
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

  it("prefers direct streaming only for a streamable selected provider", () => {
    const config = directConfig();
    config.speechPrep = undefined;
    vi.stubGlobal("AudioContext", FakeAudioContext);
    expect(
      canStreamSelectedProvider(config, {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      }),
    ).toBe(true);
    expect(canStreamSelectedProvider(config, DEFAULT_SETTINGS)).toBe(false);
  });

  it("maps PWA selections to backend job overrides", () => {
    expect(
      serverJobOptions(directConfig(), {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
        emotionPreprocessing: false,
      }),
    ).toEqual({
      provider: "elevenlabs",
      voice: "narrator",
      model: "eleven_v3",
      speechPrepEnabled: false,
    });
  });

  it("maps provider-default Google selection to the configured native voice", () => {
    expect(
      serverJobOptions(directConfig(), {
        ...DEFAULT_SETTINGS,
        provider: "google",
        voice: "provider-default",
      }).voice,
    ).toBe("Kore");
  });
});

describe("GenerationController — path selection", () => {
  it("streams a supported ElevenLabs selection before creating a server job", async () => {
    const config = directConfig();
    config.speechPrep = undefined;
    vi.stubGlobal("AudioContext", FakeAudioContext);
    const urls: string[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      urls.push(url);
      if (url === "/web/speech-jobs") throw new Error("server job must not be created");
      if (url.includes("/v1/text-to-speech/voice-1/stream")) {
        return new Response(new Uint8Array([0, 0, 1, 0]), {
          status: 200,
          headers: { "content-type": "application/octet-stream" },
        });
      }
      throw new Error(`unrouted fetch: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const audio: { provider: string; streamed?: boolean; streamingModel?: string }[] = [];
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
      callbacks: {
        onAudioReady: (_blob, meta) =>
          audio.push({
            provider: meta.provider,
            streamed: meta.streamed,
            streamingModel: meta.streamingModel,
          }),
      },
    });

    await controller.generate("Hello from the stream");

    expect(audio).toEqual([
      { provider: "elevenlabs", streamed: true, streamingModel: "eleven_v3" },
    ]);
    expect(urls).toHaveLength(1);
    expect(urls[0]).toContain("/v1/text-to-speech/voice-1/stream");
  });

  it("streams a fitted ElevenLabs v3 excerpt after over-limit prep fails", async () => {
    const config = directConfig();
    config.providers.elevenlabs!.modelId = "eleven_v3";
    config.providers.elevenlabs!.maxTextLength = 6000;
    config.speechPrep = {
      ...noPrep,
      threshold: 120,
      maxInputLength: 12000,
      maxLength: 6000,
    } as BrowserTtsConfig["speechPrep"];
    vi.stubGlobal("AudioContext", FakeAudioContext);
    const streamInputs: string[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.includes(":generateContent")) {
        return new Response("prep unavailable", { status: 400 });
      }
      if (url.includes("/v1/text-to-speech/voice-1/stream")) {
        streamInputs.push(String(JSON.parse(String(init?.body)).text));
        return new Response(new Uint8Array([0, 0, 1, 0]), { status: 200 });
      }
      if (url === "/web/speech-jobs") throw new Error("server job must not be created");
      throw new Error(`unrouted fetch: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
      callbacks: {},
    });

    await controller.generate("x".repeat(6386));

    expect(streamInputs).toHaveLength(1);
    expect(streamInputs[0]).toHaveLength(4000);
  });

  it("streams immediately after an unusably short summary without a second prep pass", async () => {
    const config = directConfig();
    config.providers.elevenlabs!.modelId = "eleven_v3";
    config.providers.elevenlabs!.maxTextLength = 6000;
    config.speechPrep = {
      ...noPrep,
      threshold: 120,
      maxInputLength: 12000,
      maxLength: 6000,
    } as BrowserTtsConfig["speechPrep"];
    vi.stubGlobal("AudioContext", FakeAudioContext);
    let prepCalls = 0;
    const streamInputs: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes(":generateContent")) {
          prepCalls += 1;
          return new Response(
            JSON.stringify({ candidates: [{ content: { parts: [{ text: "too short" }] } }] }),
            { status: 200 },
          );
        }
        if (url.includes("/v1/text-to-speech/voice-1/stream")) {
          streamInputs.push(String(JSON.parse(String(init?.body)).text));
          return new Response(new Uint8Array([0, 0, 1, 0]), { status: 200 });
        }
        if (url === "/web/speech-jobs") throw new Error("server job must not be created");
        throw new Error(`unrouted fetch: ${url}`);
      }),
    );
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
      callbacks: {},
    });

    await controller.generate("x".repeat(6386));

    expect(prepCalls).toBe(1);
    expect(streamInputs).toEqual(["x".repeat(4000)]);
  });

  it("fits an over-limit ElevenLabs v3 stream when speech prep is absent", async () => {
    const config = directConfig();
    config.providers.elevenlabs!.modelId = "eleven_v3";
    config.providers.elevenlabs!.maxTextLength = 6000;
    config.speechPrep = undefined;
    vi.stubGlobal("AudioContext", FakeAudioContext);
    const streamInputs: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes("/v1/text-to-speech/voice-1/stream")) {
          streamInputs.push(String(JSON.parse(String(init?.body)).text));
          return new Response(new Uint8Array([0, 0, 1, 0]), { status: 200 });
        }
        if (url === "/web/speech-jobs") throw new Error("server job must not be created");
        throw new Error(`unrouted fetch: ${url}`);
      }),
    );
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
      callbacks: {},
    });

    await controller.generate("x".repeat(6386));

    expect(streamInputs).toEqual(["x".repeat(4000)]);
  });

  it("does not let performance tags re-expand an ElevenLabs v3 stream past its limit", async () => {
    const config = directConfig();
    config.providers.elevenlabs!.modelId = "eleven_v3";
    config.providers.elevenlabs!.maxTextLength = 6000;
    config.speechPrep = {
      ...noPrep,
      threshold: 120,
      maxInputLength: 12000,
      maxLength: 6000,
    } as BrowserTtsConfig["speechPrep"];
    vi.stubGlobal("AudioContext", FakeAudioContext);
    const shortened = "A.".repeat(2000);
    const decorated = "[softly] A.".repeat(112) + "A.".repeat(1888);
    let prepCalls = 0;
    const streamInputs: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes(":generateContent")) {
          const text = prepCalls++ === 0 ? shortened : decorated;
          return new Response(
            JSON.stringify({ candidates: [{ content: { parts: [{ text }] } }] }),
            { status: 200 },
          );
        }
        if (url.includes("/v1/text-to-speech/voice-1/stream")) {
          streamInputs.push(String(JSON.parse(String(init?.body)).text));
          return new Response(new Uint8Array([0, 0, 1, 0]), { status: 200 });
        }
        if (url === "/web/speech-jobs") throw new Error("server job must not be created");
        throw new Error(`unrouted fetch: ${url}`);
      }),
    );
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
      callbacks: {},
    });

    await controller.generate("A.".repeat(3193));

    expect(prepCalls).toBe(2);
    expect(decorated.length).toBeGreaterThan(5000);
    expect(streamInputs).toEqual([shortened]);
  });

  it("falls back to a server job when a preferred browser stream cannot start", async () => {
    const config = directConfig();
    config.speechPrep = undefined;
    vi.stubGlobal("AudioContext", FakeAudioContext);
    const wavB64 = btoa("RIFF0000WAVEfmt ");
    const urls: string[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      urls.push(url);
      if (url.includes("/v1/text-to-speech/voice-1/stream")) {
        return new Response("stream unavailable", { status: 503 });
      }
      if (url === "/web/speech-jobs" && init?.method === "POST") {
        return new Response(JSON.stringify({ id: "job-stream-fallback" }), { status: 200 });
      }
      if (url.endsWith("/web/speech-jobs/job-stream-fallback") && init?.method === "DELETE") {
        return new Response(null, { status: 204 });
      }
      if (url.endsWith("/web/speech-jobs/job-stream-fallback")) {
        return new Response(
          JSON.stringify({
            status: "complete",
            result: {
              input: "Hello after fallback",
              input_changed: false,
              audio_base64: wavB64,
              mime_type: "audio/wav",
              format: "wav",
            },
          }),
          { status: 200 },
        );
      }
      throw new Error(`unrouted fetch: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const providers: string[] = [];
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
      callbacks: { onAudioReady: (_blob, meta) => providers.push(meta.provider) },
    });

    await controller.generate("Hello after fallback");

    expect(providers).toEqual(["server"]);
    expect(urls[0]).toContain("/v1/text-to-speech/voice-1/stream");
    expect(urls).toContain("/web/speech-jobs");
  });

  it("chunked direct path: non-streamable google synthesizes via generateContent", async () => {
    const fetchMock = routedFetch([
      {
        match: "/web/speech-jobs",
        respond: () => {
          throw new TypeError("Failed to fetch");
        },
      },
      { match: ":generateContent", respond: googleAudioResponse },
    ]);
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

  it("uses one immutable settings snapshot for the entire run", async () => {
    const config = directConfig();
    const google = config.providers.google!;
    google.fallbackModels = ["gemini-next-tts"];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const urls: string[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/web/speech-jobs") throw new TypeError("Failed to fetch");
      urls.push(url);
      if (urls.length === 1) await firstPending;
      return googleAudioResponse();
    });
    vi.stubGlobal("fetch", fetchMock);
    const controller = new GenerationController({ config, settings });

    const firstRun = controller.generate("First run");
    await vi.waitFor(() => expect(urls).toHaveLength(1));
    controller.update({ settings: { ...settings, model: "google:gemini-next-tts" } });
    releaseFirst();
    await firstRun;

    expect(urls[0]).toContain("gemini-2.5-flash-tts");
    await controller.generate("Second run");
    expect(urls[1]).toContain("gemini-next-tts");
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

  it("does not wait for completed-job cleanup before publishing audio", async () => {
    const wavB64 = btoa("RIFF0000WAVEfmt ");
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === "POST") {
        return new Response(JSON.stringify({ id: "job-cleanup" }), { status: 200 });
      }
      if (init?.method === "DELETE") return await new Promise<Response>(() => {});
      return new Response(
        JSON.stringify({
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
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    const audio: string[] = [];
    const controller = new GenerationController({
      config: null,
      settings,
      callbacks: { onAudioReady: (_blob, meta) => audio.push(meta.provider) },
    });

    await controller.generate("Hello");

    expect(audio).toEqual(["server"]);
    expect(controller.isActive).toBe(false);
    expect(fetchMock).toHaveBeenCalledWith(
      "/web/speech-jobs/job-cleanup",
      expect.objectContaining({ method: "DELETE" }),
    );
  });

  it("cancels a server job when polling fails", async () => {
    const methods: string[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      methods.push(init?.method ?? "GET");
      if (init?.method === "POST") {
        return new Response(JSON.stringify({ id: "job-failed" }), { status: 200 });
      }
      if (init?.method === "DELETE") return new Response(null, { status: 204 });
      return new Response("unavailable", { status: 503 });
    });
    vi.stubGlobal("fetch", fetchMock);
    const errors: string[] = [];
    const controller = new GenerationController({
      config: null,
      settings,
      callbacks: { onError: (message) => errors.push(message) },
    });

    await controller.generate("Hello");
    await vi.waitFor(() => expect(methods).toContain("DELETE"));

    expect(errors).toEqual(["TTS job status failed (503)"]);
    expect(loadPendingGeneration()).toBeNull();
  });

  it("uses the healthy backend before browser-direct generation", async () => {
    const wavB64 = btoa("RIFF0000WAVEfmt ");
    const fetchMock = routedFetch([
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
    const audio: string[] = [];
    const controller = new GenerationController({
      config,
      settings,
      callbacks: { onAudioReady: (_blob, meta) => audio.push(meta.provider) },
    });
    await controller.generate("Hello");
    expect(audio).toEqual(["server"]);
    expect(fetchMock).not.toHaveBeenCalledWith(
      expect.stringContaining(":generateContent"),
      expect.anything(),
    );
  });

  it("uses browser-direct generation only when backend job creation is offline", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/web/speech-jobs") throw new TypeError("Failed to fetch");
      if (url.includes(":generateContent")) return googleAudioResponse();
      throw new Error(`unrouted fetch: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const providers: string[] = [];
    const controller = new GenerationController({
      config: directConfig(),
      settings,
      callbacks: { onAudioReady: (_blob, meta) => providers.push(meta.provider) },
    });

    await controller.generate("Hello");

    expect(providers).toEqual(["google"]);
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toEqual([
      "/web/speech-jobs",
      expect.stringContaining(":generateContent"),
    ]);
  });

  it("uses browser-direct generation when Saga returns a pre-job 502", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/web/speech-jobs") return new Response("bad gateway", { status: 502 });
      if (url.includes(":generateContent")) return googleAudioResponse();
      throw new Error(`unrouted fetch: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const providers: string[] = [];
    const controller = new GenerationController({
      config: directConfig(),
      settings,
      callbacks: { onAudioReady: (_blob, meta) => providers.push(meta.provider) },
    });

    await controller.generate("Hello");

    expect(providers).toEqual(["google"]);
  });

  it("does not classify a backend HTTP 500 as offline", async () => {
    const fetchMock = vi.fn(async () => new Response("unavailable", { status: 500 }));
    vi.stubGlobal("fetch", fetchMock);
    const errors: string[] = [];
    const controller = new GenerationController({
      config: directConfig(),
      settings,
      callbacks: { onError: (message) => errors.push(message) },
    });

    await controller.generate("Hello");

    expect(errors).toEqual(["TTS job failed (500)"]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

describe("GenerationController — provider fallback ordering", () => {
  it("falls back to elevenlabs when google fails retryably under preserve-persona", async () => {
    const fetchMock = routedFetch([
      {
        match: "/web/speech-jobs",
        respond: () => {
          throw new TypeError("Failed to fetch");
        },
      },
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

  it("does not fallback when the provider is explicitly selected", async () => {
    const urls: string[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      urls.push(url);
      if (url === "/web/speech-jobs") throw new TypeError("Failed to fetch");
      if (url.includes(":generateContent")) return new Response("boom", { status: 500 });
      if (url.includes("/v1/text-to-speech/")) {
        throw new Error("explicit Google selection must not fallback to ElevenLabs");
      }
      throw new Error(`unrouted fetch: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const controller = new GenerationController({
      config: directConfig(),
      settings: {
        ...settings,
        provider: "google",
        voice: "persona:narrator",
      },
    });

    await controller.generate("Hello there");
    expect(urls.some((url) => url.includes(":generateContent"))).toBe(true);
    expect(urls.some((url) => url.includes("/v1/text-to-speech/"))).toBe(false);
  });

  it("fails before synthesis when an explicit provider lacks the selected voice backend", async () => {
    const config = directConfig();
    config.defaultProvider = "elevenlabs";
    config.personas.narrator.provider = "elevenlabs";
    config.personas.narrator.providerOrder = ["elevenlabs"];
    config.personas.narrator.google = undefined;
    const urls: string[] = [];
    const errors: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        urls.push(url);
        if (url === "/web/speech-jobs") throw new TypeError("Failed to fetch");
        throw new Error(`provider request must not be sent: ${url}`);
      }),
    );
    const controller = new GenerationController({
      config,
      settings: {
        ...settings,
        provider: "google",
        voice: "persona:narrator",
      },
      callbacks: { onError: (message) => errors.push(message) },
    });

    await controller.generate("Hello there");
    expect(urls).toEqual(["/web/speech-jobs"]);
    expect(errors).toEqual(["Selected voice has no google backend."]);
  });
});

describe("GenerationController — cancellation", () => {
  it("cancelling a direct stream preserves another owner's pending server job", async () => {
    const config = directConfig();
    config.speechPrep = undefined;
    vi.stubGlobal("AudioContext", FakeAudioContext);
    const requests: { url: string; method: string }[] = [];
    const fetchMock = vi.fn(
      (input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          requests.push({ url: String(input), method: init?.method ?? "GET" });
          init?.signal?.addEventListener("abort", () => {
            reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
          });
        }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const controller = new GenerationController({
      config,
      settings: {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
        voice: "persona:narrator",
        model: "elevenlabs:eleven_v3",
      },
      audioContextCtor: fakeAudioContextCtor,
    });

    const run = controller.generate("stream owned by this controller");
    await vi.waitFor(() =>
      expect(requests.some(({ url }) => url.includes("/v1/text-to-speech/voice-1/stream"))).toBe(
        true,
      ),
    );
    savePendingGeneration("other tab", "job-other", "other-owner");

    controller.cancel();
    await run;

    expect(loadPendingGeneration()?.jobId).toBe("job-other");
    expect(requests).not.toContainEqual(
      expect.objectContaining({ url: "/web/speech-jobs/job-other", method: "DELETE" }),
    );
  });

  it("does not let stale cleanup stop a replacement stream", () => {
    const changes: Array<StreamingPlayback | null> = [];
    const controller = new GenerationController({
      config: null,
      settings,
      callbacks: { onStreamPlaybackChange: (playback) => changes.push(playback) },
    });
    const stale = { stop: vi.fn() } as unknown as StreamingPlayback;
    const replacement = { stop: vi.fn() } as unknown as StreamingPlayback;
    const internals = controller as unknown as {
      activeStreamPlayback: StreamingPlayback | null;
      stopActiveStreamPlayback(expected?: StreamingPlayback): void;
    };
    internals.activeStreamPlayback = replacement;

    internals.stopActiveStreamPlayback(stale);
    expect(replacement.stop).not.toHaveBeenCalled();
    expect(changes).toEqual([]);

    internals.stopActiveStreamPlayback(replacement);
    expect(replacement.stop).toHaveBeenCalledOnce();
    expect(changes).toEqual([null]);
  });

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

  it("a cancelled run cannot clear its replacement's pending job", async () => {
    let postCount = 0;
    let releaseCancelledPoll!: () => void;
    const cancelledPollReleased = new Promise<void>((resolve) => {
      releaseCancelledPoll = resolve;
    });
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (init?.method === "POST") {
        postCount += 1;
        return new Response(JSON.stringify({ id: postCount === 1 ? "job-a" : "job-b" }), {
          status: 200,
        });
      }
      if (init?.method === "DELETE") return new Response(null, { status: 204 });
      if (url.endsWith("/job-a")) {
        await cancelledPollReleased;
        throw Object.assign(new Error("aborted"), { name: "AbortError" });
      }
      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => {
          reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
        });
      });
    });
    vi.stubGlobal("fetch", fetchMock);
    const controller = new GenerationController({ config: null, settings });

    const first = controller.generate("first");
    await vi.waitFor(() => expect(loadPendingGeneration()?.jobId).toBe("job-a"));
    controller.cancel();
    const second = controller.generate("second");
    await vi.waitFor(() => expect(loadPendingGeneration()?.jobId).toBe("job-b"));

    releaseCancelledPoll();
    await first;
    expect(loadPendingGeneration()?.jobId).toBe("job-b");

    controller.cancel();
    await second;
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
