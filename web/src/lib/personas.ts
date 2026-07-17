/**
 * Persona and provider resolution helpers.
 *
 * These pure functions (ported from app.html's `selectedPersonaName`,
 * `resolvePersona`, `resolveProvider`, `personaSupportsProvider`,
 * `firstPersonaForProvider`) are split out of the generation controller so the
 * app shell — which needs persona/provider resolution for the settings panel —
 * can import them without pulling in the speech-prep/synthesis/streaming
 * pipeline. The generation controller re-exports them for API compatibility.
 */

import type { BrowserPersonaConfig, BrowserTtsConfig } from "./config.ts";
import type { WebSettings } from "./settings.ts";

/** Whether a persona can drive a provider. Ports `personaSupportsProvider`. */
export function personaSupportsProvider(
  persona: BrowserPersonaConfig | null | undefined,
  provider: string,
): boolean {
  if (provider === "elevenlabs") return Boolean(persona?.elevenlabs?.voiceId);
  if (provider === "google") return Boolean(persona?.google?.voiceName);
  return false;
}

/** First persona supporting a provider. Ports `firstPersonaForProvider`. */
export function firstPersonaForProvider(
  config: BrowserTtsConfig | null | undefined,
  provider: string,
): string | null {
  const found = Object.entries(config?.personas || {}).find(([, persona]) =>
    personaSupportsProvider(persona, provider),
  );
  return found ? found[0] : null;
}

/** Resolve the selected persona name for a provider. Ports `selectedPersonaName`. */
export function selectedPersonaName(
  config: BrowserTtsConfig,
  settings: WebSettings,
): string | null {
  if (settings.voice === "provider-default") return null;
  if (settings.voice?.startsWith("persona:")) return settings.voice.slice("persona:".length);
  return config?.defaultPersona || null;
}

/** Resolve the persona object for a provider. Ports `resolvePersona`. */
export function resolvePersona(
  config: BrowserTtsConfig,
  settings: WebSettings,
): BrowserPersonaConfig | null {
  const name = selectedPersonaName(config, settings);
  return name && config.personas ? config.personas[name] || null : null;
}

/** Resolve the provider. Ports `resolveProvider`. */
export function resolveProvider(
  config: BrowserTtsConfig,
  persona: BrowserPersonaConfig | null,
  settings: WebSettings,
): string {
  if (settings.provider !== "auto") return settings.provider;
  return persona?.provider || config.defaultProvider;
}

/** Next configured backend after `provider`, preserving legacy cached v1 configs. */
export function nextVoiceProvider(
  persona: BrowserPersonaConfig | null | undefined,
  provider: string,
): string | null {
  if (!persona || persona.fallbackPolicy !== "preserve-persona") return null;
  const order = persona.providerOrder;
  if (!order?.length) return provider === "google" ? "elevenlabs" : "google";
  const index = order.indexOf(provider);
  return index >= 0 ? order[index + 1] || null : null;
}
