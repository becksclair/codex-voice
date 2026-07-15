/**
 * TypeScript types and helpers for the `/web/config` payload.
 *
 * The types mirror the serde structs in
 * `crates/codex-voice-transcriber/src/server/web.rs` (`BrowserTtsConfig` and
 * friends), which serialize with `rename_all = "camelCase"`. Fields declared
 * with `#[serde(skip_serializing_if = "Option::is_none")]` are optional here.
 *
 * The functions own the refresh-capable browser config cache and the
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
 * Cached Codex OAuth credentials. The server supplies the initial bundle and
 * the browser persists rotated credentials for backend-independent refresh.
 */
export interface BrowserCodexAuth {
  accessToken?: string;
  refreshToken?: string;
  accountId?: string;
  tokenUrl?: string;
  clientId?: string;
  expiresAt?: number;
  serverSyncPending?: boolean;
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
  /** Refresh-capable Codex auth cached for browser-direct speech prep. */
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

function accessTokenExpiry(auth: BrowserCodexAuth | null | undefined): number | null {
  try {
    const segment = String(auth?.accessToken || "").split(".")[1];
    if (!segment) return null;
    const normalized = segment.replace(/-/g, "+").replace(/_/g, "/");
    const padded = normalized + "=".repeat((4 - (normalized.length % 4)) % 4);
    const exp = Number((JSON.parse(atob(padded)) as { exp?: unknown }).exp);
    return Number.isFinite(exp) ? exp : null;
  } catch {
    return null;
  }
}

function completeCodexAuth(auth: BrowserCodexAuth | null | undefined): auth is BrowserCodexAuth {
  return Boolean(auth?.accessToken && auth.refreshToken && auth.accountId);
}

/**
 * Merge a live config over the cached config without rolling back a token
 * bundle refreshed by the browser. Account changes always take the live
 * bundle; otherwise the access token with the later JWT expiry wins, with the
 * cached bundle winning ties so a rotated refresh token is retained.
 */
export function reconcileBrowserConfig(
  fresh: BrowserTtsConfig,
  cached: BrowserTtsConfig | null | undefined,
): BrowserTtsConfig {
  const freshAuth = fresh.speechPrep?.codexAuth;
  const cachedAuth = cached?.speechPrep?.codexAuth;
  if (!completeCodexAuth(cachedAuth)) return fresh;
  if (!completeCodexAuth(freshAuth)) return fresh;
  if (freshAuth.accountId !== cachedAuth.accountId) return fresh;
  const freshExpiry = accessTokenExpiry(freshAuth);
  const cachedExpiry = accessTokenExpiry(cachedAuth);
  if (cachedExpiry !== null && (freshExpiry === null || cachedExpiry >= freshExpiry)) {
    fresh.speechPrep!.codexAuth = cachedAuth;
  }
  return fresh;
}

/**
 * Load a cached config from localStorage under {@link CONFIG_STORAGE_KEY}.
 *
 * Parses the stored JSON, including refresh-capable Codex credentials. Returns
 * `null` on absence or parse failure.
 */
export function loadCachedConfig(): BrowserTtsConfig | null {
  try {
    const raw = localStorage.getItem(CONFIG_STORAGE_KEY);
    return raw ? (JSON.parse(raw) as BrowserTtsConfig) : null;
  } catch {
    return null;
  }
}

/**
 * Persist a config to localStorage under {@link CONFIG_STORAGE_KEY}.
 *
 * The config is stored as-is, including Codex OAuth credentials, so an
 * installed PWA can reload while the backend is unavailable.
 */
export function saveCachedConfig(config: BrowserTtsConfig): void {
  localStorage.setItem(CONFIG_STORAGE_KEY, JSON.stringify(config));
}

/**
 * Best-effort synchronization of a browser-rotated Codex bundle back to the
 * canonical server auth file. Failed attempts remain pending in origin
 * storage and are retried after config recovery or before the next server job.
 */
export async function syncCodexAuthToServer(
  config: BrowserTtsConfig | null | undefined,
  signal: AbortSignal | null = null,
): Promise<boolean> {
  if (!config) return false;
  const auth = config.speechPrep?.codexAuth;
  if (!completeCodexAuth(auth) || !auth.serverSyncPending) return false;
  try {
    const response = await fetch("/web/codex-auth", {
      method: "POST",
      signal,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        accessToken: auth.accessToken,
        refreshToken: auth.refreshToken,
        accountId: auth.accountId,
      }),
    });
    if (!response.ok) return false;
    auth.serverSyncPending = false;
    saveCachedConfig(config);
    return true;
  } catch {
    return false;
  }
}

/**
 * Fetch and validate the live config from `/web/config`.
 *
 * Ports the network + validation half of `refreshConfig` (app.html line
 * ~1025). Uses `cache: 'no-store'` and requires
 * `version === 1` with a `providers` object; returns `null` on a non-OK
 * response, invalid payload, or network error. Persisting and reconciling the result and
 * repopulating settings is left to the caller (B2), matching the original
 * split of responsibilities.
 */
export async function fetchConfig(): Promise<BrowserTtsConfig | null> {
  try {
    const response = await fetch("/web/config", { cache: "no-store" });
    if (!response.ok) return null;
    const config = (await response.json()) as BrowserTtsConfig;
    if (config?.version !== 1 || !config.providers) return null;
    return config;
  } catch {
    return null;
  }
}
