import { useEffect, useRef, useState } from "react";
import { useLatest } from "./useLatest.ts";
import {
  applyTheme,
  firstPersonaForProvider,
  loadSettings,
  personaSupportsProvider,
  saveSettings as persistSettings,
  SETTINGS_STORAGE_KEY,
  type BrowserTtsConfig,
  type ThemePreference,
  type WebSettings,
} from "../lib/index.ts";

/** A `<select>` option: its stored value and human label. */
export interface SelectOption {
  value: string;
  label: string;
}

function providerCanGenerate(config: BrowserTtsConfig | null, provider: string): boolean {
  if (provider === "google") return Boolean(config?.providers?.google);
  if (provider === "elevenlabs") {
    return Boolean(config?.providers?.elevenlabs && firstPersonaForProvider(config, "elevenlabs"));
  }
  return false;
}

function personaEntries(config: BrowserTtsConfig | null) {
  return Object.entries(config?.personas || {});
}

function providerModelValues(config: BrowserTtsConfig | null, provider: string): string[] {
  const seen = new Set<string>();
  const values: string[] = [];
  const push = (value: string | undefined | null): void => {
    if (!value || seen.has(value)) return;
    seen.add(value);
    values.push(value);
  };
  if (provider === "google") {
    const google = config?.providers?.google;
    push(google?.model);
    for (const model of google?.fallbackModels || []) push(model);
  } else if (provider === "elevenlabs") {
    push(config?.providers?.elevenlabs?.modelId);
  }
  return values;
}

function providerOptionsFor(config: BrowserTtsConfig | null): SelectOption[] {
  const options: SelectOption[] = [{ value: "auto", label: "Auto" }];
  if (providerCanGenerate(config, "google")) options.push({ value: "google", label: "Google" });
  if (providerCanGenerate(config, "elevenlabs")) {
    options.push({ value: "elevenlabs", label: "ElevenLabs" });
  }
  return options;
}

function voiceOptionsFor(config: BrowserTtsConfig | null, provider: string): SelectOption[] {
  const options: SelectOption[] = [{ value: "default", label: "Default" }];
  if (provider !== "elevenlabs") {
    options.push({ value: "provider-default", label: "Provider default" });
  }
  for (const [name, persona] of personaEntries(config)) {
    if (!personaSupportsProvider(persona, provider)) continue;
    options.push({ value: `persona:${name}`, label: persona.label || name });
  }
  return options;
}

function modelOptionsFor(config: BrowserTtsConfig | null, provider: string): SelectOption[] {
  const options: SelectOption[] = [{ value: "default", label: "Default" }];
  if (provider !== "auto") {
    for (const model of providerModelValues(config, provider)) {
      options.push({ value: `${provider}:${model}`, label: model });
    }
  }
  return options;
}

function has(options: SelectOption[], value: string): boolean {
  return options.some((option) => option.value === value);
}

/** The reconciled option lists and clamped selection for the current config. */
interface Reconciled {
  provider: string;
  voice: string;
  model: string;
  providerOptions: SelectOption[];
  voiceOptions: SelectOption[];
  modelOptions: SelectOption[];
}

/**
 * Clamp the stored provider/voice/model to what the live config supports.
 *
 * Ports `populateSettings` (app.html): build the option lists from the config,
 * keep each prior selection when it is still valid, otherwise fall back to the
 * neutral default. Voice and model options depend on the reconciled provider.
 */
function reconcile(settings: WebSettings, config: BrowserTtsConfig | null): Reconciled {
  const providerOptions = providerOptionsFor(config);
  const provider = has(providerOptions, settings.provider) ? settings.provider : "auto";
  const voiceOptions = voiceOptionsFor(config, provider);
  const voice = has(voiceOptions, settings.voice) ? settings.voice : "default";
  const modelOptions = modelOptionsFor(config, provider);
  const model = has(modelOptions, settings.model) ? settings.model : "default";
  return { provider, voice, model, providerOptions, voiceOptions, modelOptions };
}

/** The public surface of {@link useSettings}. */
export interface SettingsState extends Reconciled {
  /** The full persisted settings object (with reconciled provider/voice/model). */
  settings: WebSettings;
  setProvider: (value: string) => void;
  setVoice: (value: string) => void;
  setModel: (value: string) => void;
  setTheme: (value: ThemePreference) => void;
  setEmotion: (value: boolean) => void;
  setSummarize: (value: boolean) => void;
  setGenerateOnPaste: (value: boolean) => void;
}

/**
 * Owns user settings: persistence, theme application, and the config-driven
 * reconciliation of the provider/voice/model selects.
 *
 * Replaces the imperative `loadSettings`/`applySettingsToForm`/`populateSettings`/
 * `saveSettings` block from the legacy mount effect. Settings are persisted on
 * every change; the theme is applied whenever the preference (or the OS
 * preference, while on `auto`) changes; and the selects reconcile against the
 * live config as it loads.
 */
export function useSettings(config: BrowserTtsConfig | null): SettingsState {
  const [settings, setSettings] = useState(loadSettings);

  const mediaRef = useRef<MediaQueryList | null>(null);
  if (mediaRef.current === null && typeof window !== "undefined") {
    mediaRef.current = window.matchMedia?.("(prefers-color-scheme: light)") ?? null;
  }

  const reconciled = reconcile(settings, config);

  const commit = (patch: Partial<WebSettings>): void => {
    setSettings((prev) => ({ ...prev, ...patch }));
  };

  // Persist on every settings change (mirrors `saveSettings`, which always
  // wrote the object back — including the initial load).
  useEffect(() => {
    persistSettings(settings);
  }, [settings]);

  // Apply the theme whenever the preference changes.
  useEffect(() => {
    applyTheme(document, settings.theme || "auto", Boolean(mediaRef.current?.matches));
  }, [settings.theme]);

  // While on `auto`, follow the OS color-scheme changes.
  useEffect(() => {
    const media = mediaRef.current;
    if (!media?.addEventListener) return;
    const onChange = (): void => {
      if ((settings.theme || "auto") === "auto") applyTheme(document, "auto", media.matches);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [settings.theme]);

  // Cross-window sync: another window (e.g. a second Tauri webview) may write
  // SETTINGS_STORAGE_KEY. `storage` fires in other same-origin documents on
  // every write; WebKitGTK is not reliable about firing it across separate
  // webviews, so `focus` is a fallback re-read. Both paths compare against the
  // current in-memory settings before calling `setSettings`, so a no-op
  // external read never triggers a new persist write (which would otherwise
  // re-fire `storage` in the other window and ping-pong).
  const settingsRefForSync = useLatest(settings);
  useEffect(() => {
    const applyExternal = (): void => {
      const next = loadSettings();
      if (JSON.stringify(next) === JSON.stringify(settingsRefForSync.current)) return;
      setSettings(next);
    };
    const onStorage = (event: StorageEvent): void => {
      if (event.key !== null && event.key !== SETTINGS_STORAGE_KEY) return;
      applyExternal();
    };
    window.addEventListener("storage", onStorage);
    window.addEventListener("focus", applyExternal);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("focus", applyExternal);
    };
    // `settingsRefForSync` is a stable ref, so this still mounts exactly once.
  }, [settingsRefForSync]);

  // Reconcile the stored selection against the live config; persist the clamp.
  // Skip while config is still null: reconciling against no config would clamp
  // provider/voice/model down to the neutral defaults and persist that,
  // destroying the user's real selection and (via cross-window sync) pushing
  // the downgrade into a sibling window that did load config. The display
  // `reconciled` above still clamps for show; we just never persist it until
  // config is known.
  useEffect(() => {
    if (!config) return;
    const next = reconcile(settings, config);
    if (
      next.provider !== settings.provider ||
      next.voice !== settings.voice ||
      next.model !== settings.model
    ) {
      setSettings((prev) => ({
        ...prev,
        provider: next.provider,
        voice: next.voice,
        model: next.model,
      }));
    }
  }, [config, settings]);

  return {
    settings,
    ...reconciled,
    setProvider: (value) => commit({ provider: value }),
    setVoice: (value) => commit({ voice: value }),
    setModel: (value) => commit({ model: value }),
    setTheme: (value) => commit({ theme: value }),
    setEmotion: (value) => commit({ emotionPreprocessing: value }),
    setSummarize: (value) => commit({ summarization: value }),
    setGenerateOnPaste: (value) => commit({ generateOnPaste: value }),
  };
}
