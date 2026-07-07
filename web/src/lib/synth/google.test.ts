import { afterEach, describe, expect, it, vi } from "vitest";
import type { BrowserGoogleConfig, BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import { ProviderError } from "./common.ts";
import {
  buildGoogleTtsPrompt,
  fetchGoogleAudio,
  normalizeGoogleModelName,
  resolveGoogleModel,
  synthesizeGoogle,
} from "./google.ts";

const google: BrowserGoogleConfig = {
  apiKey: "g-key",
  baseUrl: "https://gl.example/v1beta",
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
};

const config = { providers: { google } } as BrowserTtsConfig;

const persona: BrowserPersonaConfig = {
  label: "Narrator",
  description: "",
  provider: "google",
  fallbackPolicy: "preserve-persona",
  promptScene: "quiet room",
  promptStyle: "warm",
  promptPacing: "measured",
  promptConstraints: ["no ad-libs"],
  google: { voiceName: "Charon" },
};

/** base64 for a small PCM payload. */
const pcmBase64 = btoa(String.fromCharCode(0, 64, 0, 128));

function googleAudioResponse(mimeType: string, dataBase64: string): Response {
  return new Response(
    JSON.stringify({
      candidates: [{ content: { parts: [{ inlineData: { data: dataBase64, mimeType } }] } }],
    }),
    { status: 200 },
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("normalizeGoogleModelName", () => {
  it("strips a leading google/ prefix", () => {
    expect(normalizeGoogleModelName("google/gemini-2.5-flash-tts")).toBe("gemini-2.5-flash-tts");
    expect(normalizeGoogleModelName(undefined)).toBe("");
  });
});

describe("resolveGoogleModel", () => {
  it("uses the config default without a settings selection", () => {
    expect(resolveGoogleModel(google)).toBe("gemini-2.5-flash-tts");
    expect(resolveGoogleModel(google, "default")).toBe("gemini-2.5-flash-tts");
  });

  it("uses the provider-prefixed settings model", () => {
    expect(resolveGoogleModel(google, "google:gemini-2.5-pro-tts")).toBe("gemini-2.5-pro-tts");
  });

  it("returns empty for a missing provider", () => {
    expect(resolveGoogleModel(null)).toBe("");
  });
});

describe("buildGoogleTtsPrompt", () => {
  it("includes the delivery profile and guardrails", () => {
    const prompt = buildGoogleTtsPrompt("Read me", persona, "smile");
    expect(prompt).toContain("- scene: quiet room");
    expect(prompt).toContain("- style: warm");
    expect(prompt).toContain("- pace: measured");
    expect(prompt).toContain("- constraint: no ad-libs");
    expect(prompt).toContain("Additional delivery hints:\n- smile");
    expect(prompt).toContain("- speak the text exactly as written");
    expect(prompt.endsWith('Text:\n"""Read me"""')).toBe(true);
  });

  it("omits the profile without a persona", () => {
    const prompt = buildGoogleTtsPrompt("hi", null, null);
    expect(prompt).not.toContain("Delivery profile");
    expect(prompt.startsWith("Read the following text aloud.")).toBe(true);
  });
});

describe("fetchGoogleAudio", () => {
  it("issues the expected request and parses inline audio", async () => {
    const fetchMock = vi.fn(async () =>
      googleAudioResponse("audio/L16;codec=pcm;rate=24000", pcmBase64),
    );
    vi.stubGlobal("fetch", fetchMock);
    const audio = await fetchGoogleAudio(config, "hello", persona, null);
    expect([...audio.bytes]).toEqual([0, 64, 0, 128]);
    expect(audio.mimeType).toBe("audio/L16;codec=pcm;rate=24000");

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("https://gl.example/v1beta/models/gemini-2.5-flash-tts:generateContent");
    const headers = init.headers as Record<string, string>;
    expect(headers["x-goog-api-key"]).toBe("g-key");
    const body = JSON.parse(init.body as string);
    expect(body.generationConfig.responseModalities).toEqual(["AUDIO"]);
    expect(body.generationConfig.speechConfig.voiceConfig.prebuiltVoiceConfig.voiceName).toBe(
      "Charon",
    );
    expect(body.contents[0].parts[0].text).toContain("quiet room");
  });

  it("honors the model option in the request URL", async () => {
    const fetchMock = vi.fn(async () => googleAudioResponse("audio/wav", pcmBase64));
    vi.stubGlobal("fetch", fetchMock);
    await fetchGoogleAudio(config, "hello", null, null, { model: "gemini-2.5-pro-tts" });
    expect((fetchMock.mock.calls[0] as unknown as [string])[0]).toContain(":generateContent");
    expect((fetchMock.mock.calls[0] as unknown as [string])[0]).toContain("gemini-2.5-pro-tts");
  });

  it("throws a ProviderError with status on failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("quota", { status: 429 })),
    );
    const error = await fetchGoogleAudio(config, "x", null, null).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ProviderError);
    expect((error as ProviderError).status).toBe(429);
    expect((error as ProviderError).message).toBe("Google TTS failed: quota");
  });

  it("throws when no audio is returned", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ candidates: [] }), { status: 200 })),
    );
    await expect(fetchGoogleAudio(config, "x", null, null)).rejects.toThrow(
      "Google TTS returned no audio.",
    );
  });
});

describe("synthesizeGoogle", () => {
  it("wraps a short PCM response as WAV", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleAudioResponse("audio/L16;codec=pcm;rate=24000", pcmBase64)),
    );
    const blob = await synthesizeGoogle(config, "hello", persona, null);
    const bytes = new Uint8Array(await blob.arrayBuffer());
    expect(String.fromCharCode(...bytes.slice(0, 4))).toBe("RIFF");
    expect(blob.type).toBe("audio/wav");
  });

  it("returns non-PCM audio as a raw blob", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => googleAudioResponse("audio/mpeg", pcmBase64)),
    );
    const blob = await synthesizeGoogle(config, "hello", null, null);
    expect(blob.type).toBe("audio/mpeg");
  });
});
