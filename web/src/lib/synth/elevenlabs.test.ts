import { afterEach, describe, expect, it, vi } from "vitest";
import type { BrowserElevenLabsConfig, BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import { ProviderError } from "./common.ts";
import {
  elevenLabsMimeType,
  elevenLabsSampleRate,
  elevenLabsWebSocketModelSupported,
  resolveElevenLabsModel,
  resolveElevenLabsSpeed,
  synthesizeElevenLabsSingle,
  websocketBaseUrl,
} from "./elevenlabs.ts";

const elevenlabs: BrowserElevenLabsConfig = {
  apiKey: "el-key",
  baseUrl: "https://el.example",
  modelId: "eleven_flash_v2_5",
  streaming: {
    transport: "websocket",
    preferredModel: "eleven_flash_v2_5",
    outputFormat: "pcm_24000",
    sampleRate: 24000,
    channels: 1,
    chunkLengthSchedule: [120, 160],
  },
  applyTextNormalization: "auto",
  outputFormat: "mp3_44100_128",
  streamGain: 2,
  languageCode: "en",
  maxTextLength: 5000,
  timeoutMs: 30000,
};

const config = { providers: { elevenlabs } } as BrowserTtsConfig;

const persona: BrowserPersonaConfig = {
  label: "Reader",
  description: "",
  provider: "elevenlabs",
  fallbackPolicy: "preserve-persona",
  promptConstraints: [],
  elevenlabs: {
    voiceId: "voice-123",
    voiceSettings: {
      stability: 0.5,
      similarityBoost: 0.7,
      style: 0.1,
      useSpeakerBoost: true,
      speed: 1.5, // out of range, clamps to 1.2
    },
  },
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("model + format helpers", () => {
  it("resolveElevenLabsModel uses default or settings selection", () => {
    expect(resolveElevenLabsModel(elevenlabs)).toBe("eleven_flash_v2_5");
    expect(resolveElevenLabsModel(elevenlabs, "elevenlabs:eleven_v3")).toBe("eleven_v3");
    expect(resolveElevenLabsModel(null)).toBe("");
  });

  it("resolveElevenLabsSpeed clamps to [0.7, 1.2]", () => {
    expect(resolveElevenLabsSpeed(persona)).toBe(1.2);
    expect(resolveElevenLabsSpeed(null)).toBe(1.0);
  });

  it("elevenLabsMimeType maps output formats", () => {
    expect(elevenLabsMimeType("wav_24000")).toBe("audio/wav");
    expect(elevenLabsMimeType("pcm_24000")).toBe("audio/pcm");
    expect(elevenLabsMimeType("opus_48000")).toBe("audio/opus");
    expect(elevenLabsMimeType("mp3_44100_128")).toBe("audio/mpeg");
  });

  it("elevenLabsSampleRate parses pcm_<rate>", () => {
    expect(elevenLabsSampleRate("pcm_16000")).toBe(16000);
    expect(elevenLabsSampleRate("mp3_44100_128")).toBe(24000);
  });

  it("elevenLabsWebSocketModelSupported excludes eleven_v3", () => {
    expect(elevenLabsWebSocketModelSupported("eleven_flash_v2_5")).toBe(true);
    expect(elevenLabsWebSocketModelSupported("eleven_v3")).toBe(false);
    expect(elevenLabsWebSocketModelSupported("")).toBe(false);
  });

  it("websocketBaseUrl swaps to wss and strips trailing slash", () => {
    expect(websocketBaseUrl("https://api.elevenlabs.io/")).toBe("wss://api.elevenlabs.io");
    expect(websocketBaseUrl("http://localhost:8080")).toBe("ws://localhost:8080");
  });
});

describe("synthesizeElevenLabsSingle", () => {
  it("issues the expected request with clamped speed", async () => {
    const fetchMock = vi.fn(
      async () =>
        new Response(new Uint8Array([1, 2, 3, 4]), {
          status: 200,
          headers: { "content-type": "audio/mpeg" },
        }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const blob = await synthesizeElevenLabsSingle(config, "hello", persona);
    expect(blob.type).toBe("audio/mpeg");

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("https://el.example/v1/text-to-speech/voice-123?output_format=mp3_44100_128");
    const headers = init.headers as Record<string, string>;
    expect(headers["xi-api-key"]).toBe("el-key");
    const body = JSON.parse(init.body as string);
    expect(body.text).toBe("hello");
    expect(body.model_id).toBe("eleven_flash_v2_5");
    expect(body.voice_settings.speed).toBe(1.2);
    expect(body.apply_text_normalization).toBe("auto");
    expect(body.language_code).toBe("en");
  });

  it("wraps PCM output as WAV unless rawPcm is set", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(new Uint8Array([9, 9, 9, 9]), { status: 200 })),
    );
    const blob = await synthesizeElevenLabsSingle(config, "hi", persona, {
      outputFormat: "pcm_24000",
    });
    const bytes = new Uint8Array(await blob.arrayBuffer());
    expect(String.fromCharCode(...bytes.slice(0, 4))).toBe("RIFF");
  });

  it("returns raw PCM bytes when rawPcm is true", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(new Uint8Array([9, 9, 9, 9]), { status: 200 })),
    );
    const blob = await synthesizeElevenLabsSingle(config, "hi", persona, {
      outputFormat: "pcm_24000",
      rawPcm: true,
    });
    const bytes = new Uint8Array(await blob.arrayBuffer());
    expect([...bytes]).toEqual([9, 9, 9, 9]);
  });

  it("throws a ProviderError on failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("bad", { status: 401 })),
    );
    const error = await synthesizeElevenLabsSingle(config, "x", persona).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ProviderError);
    expect((error as ProviderError).status).toBe(401);
  });

  it("throws when the persona has no voiceId", async () => {
    const noVoice = { ...persona, elevenlabs: undefined };
    await expect(synthesizeElevenLabsSingle(config, "x", noVoice)).rejects.toThrow(
      "ElevenLabs voice_id is not configured for this persona.",
    );
  });
});
