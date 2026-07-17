import { describe, expect, test } from "vitest";
import type { BrowserPersonaConfig, BrowserTtsConfig } from "./config.ts";
import { nextVoiceProvider, personaSupportsProvider, selectedPersonaName } from "./personas.ts";
import { DEFAULT_SETTINGS } from "./settings.ts";

function persona(overrides: Partial<BrowserPersonaConfig> = {}): BrowserPersonaConfig {
  return {
    label: "Sky",
    description: "Test voice",
    provider: "elevenlabs",
    fallbackPolicy: "preserve-persona",
    providerOrder: ["elevenlabs", "google"],
    promptConstraints: [],
    google: { voiceName: "Sulafat" },
    elevenlabs: {
      voiceId: "voice-id",
      voiceSettings: {
        stability: 0.5,
        similarityBoost: 0.75,
        style: 0,
        useSpeakerBoost: true,
        speed: 1,
      },
    },
    ...overrides,
  };
}

test("ordered backend fallback never loops back to an earlier provider", () => {
  const voice = persona();
  expect(nextVoiceProvider(voice, "elevenlabs")).toBe("google");
  expect(nextVoiceProvider(voice, "google")).toBeNull();
  expect(nextVoiceProvider(voice, "unknown")).toBeNull();
});

test("legacy cached config retains opposite-provider fallback", () => {
  const voice = persona({ providerOrder: undefined });
  expect(nextVoiceProvider(voice, "elevenlabs")).toBe("google");
  expect(nextVoiceProvider(voice, "google")).toBe("elevenlabs");
});

describe("selectedPersonaName", () => {
  test("does not substitute another voice for an unsupported explicit provider", () => {
    const config = {
      defaultPersona: "google-only",
      personas: {
        "google-only": persona({ elevenlabs: undefined, providerOrder: ["google"] }),
        fallback: persona(),
      },
    } as unknown as BrowserTtsConfig;
    expect(
      selectedPersonaName(config, {
        ...DEFAULT_SETTINGS,
        provider: "elevenlabs",
      }),
    ).toBe("google-only");
    expect(personaSupportsProvider(config.personas["google-only"], "elevenlabs")).toBe(false);
  });
});
