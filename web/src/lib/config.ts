/**
 * TypeScript types and helpers for the `/web/config` payload.
 *
 * The types mirror the serde structs in
 * `crates/codex-voice-transcriber/src/server/web.rs` (`BrowserTtsConfig` and
 * friends), which serialize with `rename_all = "camelCase"`. Fields declared
 * with `#[serde(skip_serializing_if = "Option::is_none")]` are optional here.
 *
 * The functions port `sanitizeBrowserConfig`, `loadCachedConfig`, and the
 * config-fetching portion of `refreshConfig` from app.html.
 */

import { CONFIG_STORAGE_KEY } from "./storage.ts";

/** Google streaming sub-config (`BrowserGoogleStreamingConfig`). */
export interface BrowserGoogleStreamingConfig {
  transport: string;
  supportedModels: string[];
  outputFormat: string;
  sampleRate: number;
  channels: number;
}

/** Google provider config (`BrowserGoogleConfig`). */
export interface BrowserGoogleConfig {
  apiKey: string;
  baseUrl: string;
  voice: string;
  model: string;
  fallbackModels: string[];
  streaming: BrowserGoogleStreamingConfig;
  inlineAudioTags?: boolean;
  maxTextLength: number;
  timeoutMs: number;
  scene?: string;
  sampleContext?: string;
  style?: string;
  pace?: string;
  constraints: string[];
}

/** ElevenLabs streaming sub-config (`BrowserElevenLabsStreamingConfig`). */
export interface BrowserElevenLabsStreamingConfig {
  transport: string;
  preferredModel: string;
  outputFormat: string;
  sampleRate: number;
  channels: number;
  chunkLengthSchedule: number[];
}

/** ElevenLabs provider config (`BrowserElevenLabsConfig`). */
export interface BrowserElevenLabsConfig {
  apiKey: string;
  baseUrl: string;
  modelId: string;
  streaming: BrowserElevenLabsStreamingConfig;
  applyTextNormalization: string;
  outputFormat: string;
  streamGain: number;
  languageCode?: string;
  inlineAudioTags?: boolean;
  maxTextLength: number;
  timeoutMs: number;
}

/** Provider map (`BrowserProviders`). */
export interface BrowserProviders {
  google?: BrowserGoogleConfig;
  elevenlabs?: BrowserElevenLabsConfig;
}

/** Speech-prep strategy names per provider (`BrowserSpeechPrepStrategies`). */
export interface BrowserSpeechPrepStrategies {
  google: string;
  elevenlabs: string;
  default: string;
}

/** Speech-prep browser fallback (`BrowserSpeechPrepFallbackConfig`). */
export interface BrowserSpeechPrepFallbackConfig {
  provider: string;
  apiKey: string;
  baseUrl: string;
  model: string;
  fallbackModels: string[];
}

/**
 * Cached Codex OAuth credentials. The server never serializes this — it is
 * injected client-side after a token refresh (app.html line ~2449) and is
 * stripped again by {@link sanitizeBrowserConfig} before persisting/using.
 */
export interface BrowserCodexAuth {
  accessToken?: string;
  refreshToken?: string;
  tokenUrl?: string;
  clientId?: string;
  expiresAt?: number;
  [key: string]: unknown;
}

/** Speech-prep config (`BrowserSpeechPrepConfig`). */
export interface BrowserSpeechPrepConfig {
  provider: string;
  mode: string;
  strategies: BrowserSpeechPrepStrategies;
  tagPalette: string[];
  capPerformanceTags: boolean;
  browserSupported: boolean;
  browserFallback?: BrowserSpeechPrepFallbackConfig;
  apiKey?: string;
  baseUrl: string;
  model: string;
  fallbackModels: string[];
  reasoningEffort?: string;
  threshold: number;
  maxInputLength: number;
  maxLength: number;
  attemptTimeoutMs: number;
  timeoutMs: number;
  /** Client-only cached Codex auth; stripped by {@link sanitizeBrowserConfig}. */
  codexAuth?: BrowserCodexAuth;
}

/** Google persona overrides (`BrowserGooglePersonaConfig`). */
export interface BrowserGooglePersonaConfig {
  voiceName: string;
}

/** ElevenLabs voice settings (`BrowserElevenLabsVoiceSettings`). */
export interface BrowserElevenLabsVoiceSettings {
  stability: number;
  similarityBoost: number;
  style: number;
  useSpeakerBoost: boolean;
  speed: number;
}

/** ElevenLabs persona overrides (`BrowserElevenLabsPersonaConfig`). */
export interface BrowserElevenLabsPersonaConfig {
  voiceId: string;
  voiceSettings: BrowserElevenLabsVoiceSettings;
}

/** A resolved persona (`BrowserPersonaConfig`). */
export interface BrowserPersonaConfig {
  label: string;
  description: string;
  provider: string;
  fallbackPolicy: string;
  promptScene?: string;
  promptSampleContext?: string;
  promptStyle?: string;
  promptPacing?: string;
  promptConstraints: string[];
  google?: BrowserGooglePersonaConfig;
  elevenlabs?: BrowserElevenLabsPersonaConfig;
}

/** Top-level `/web/config` payload (`BrowserTtsConfig`). */
export interface BrowserTtsConfig {
  version: number;
  defaultProvider: string;
  defaultPersona?: string;
  maxTextLength: number;
  providers: BrowserProviders;
  speechPrep?: BrowserSpeechPrepConfig;
  personas: Record<string, BrowserPersonaConfig>;
}

/**
 * Strip the client-only `speechPrep.codexAuth` field in place and return the
 * same object.
 *
 * Ports `sanitizeBrowserConfig` (app.html line ~873). Only `codexAuth` is
 * removed; every other field is left untouched. Accepts `unknown` because it
 * runs on freshly-parsed JSON and cached values that may be malformed.
 */
export function sanitizeBrowserConfig<T>(config: T): T {
  const candidate = config as { speechPrep?: { codexAuth?: unknown } } | null | undefined;
  if (candidate?.speechPrep?.codexAuth) {
    delete candidate.speechPrep.codexAuth;
  }
  return config;
}

/**
 * Load a cached config from localStorage under {@link CONFIG_STORAGE_KEY}.
 *
 * Ports `loadCachedConfig` (app.html line ~880): parses the stored JSON,
 * sanitizes it, and re-persists the sanitized form (so any lingering
 * `codexAuth` is scrubbed from storage). Returns `null` on absence or parse
 * failure.
 */
export function loadCachedConfig(): BrowserTtsConfig | null {
  try {
    const raw = localStorage.getItem(CONFIG_STORAGE_KEY);
    const config = raw ? sanitizeBrowserConfig(JSON.parse(raw) as BrowserTtsConfig) : null;
    if (config) localStorage.setItem(CONFIG_STORAGE_KEY, JSON.stringify(config));
    return config;
  } catch {
    return null;
  }
}

/**
 * Persist a config to localStorage under {@link CONFIG_STORAGE_KEY}.
 *
 * Mirrors the `localStorage.setItem(configStorageKey, ...)` calls in app.html
 * (`refreshConfig`, `loadCachedConfig`). The config is stored as-is; callers
 * should sanitize first if the value may contain `codexAuth`.
 */
export function saveCachedConfig(config: BrowserTtsConfig): void {
  localStorage.setItem(CONFIG_STORAGE_KEY, JSON.stringify(config));
}

/**
 * Fetch and validate the live config from `/web/config`.
 *
 * Ports the network + validation half of `refreshConfig` (app.html line
 * ~1025). Uses `cache: 'no-store'`, sanitizes the payload, and requires
 * `version === 1` with a `providers` object; returns `null` on a non-OK
 * response, invalid payload, or network error. Persisting the result and
 * repopulating settings is left to the caller (B2), matching the original
 * split of responsibilities.
 */
export async function fetchConfig(): Promise<BrowserTtsConfig | null> {
  try {
    const response = await fetch("/web/config", { cache: "no-store" });
    if (!response.ok) return null;
    const config = sanitizeBrowserConfig((await response.json()) as BrowserTtsConfig);
    if (config?.version !== 1 || !config.providers) return null;
    return config;
  } catch {
    return null;
  }
}
