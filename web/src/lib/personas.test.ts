import { describe, expect, test } from "vitest";
import type { BrowserPersonaConfig, BrowserTtsConfig } from "./config.ts";
import { nextVoiceProvider, personaSupportsProvider, selectedPersonaName } from "./personas.ts";
import { DEFAULT_SETTINGS } from "./settings.ts";

function persona(overrides: Partial<BrowserPersonaConfig> = {}): BrowserPersonaConfig {
  return {
    label: "Sky",
    description: "Test voice",
    provider: "elevenlabs",
    providerOrder: ["elevenlabs", "google"],
    ...overrides,
  };
}

test("ordered backend fallback never loops back to an earlier provider", () => {
  const voice = persona();
  expect(nextVoiceProvider(voice, "elevenlabs")).toBe("google");
  expect(nextVoiceProvider(voice, "google")).toBeNull();
  expect(nextVoiceProvider(voice, "unknown")).toBeNull();
});

describe("selectedPersonaName", () => {
  test("does not substitute another voice for an unsupported explicit provider", () => {
    const config = {
      defaultPersona: "google-only",
      personas: {
        "google-only": persona({ providerOrder: ["google"] }),
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
