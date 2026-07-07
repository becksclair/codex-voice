/**
 * User settings shape, defaults, and load/save/sanitize helpers.
 *
 * Ports `loadSettings`/`saveSettings` and the defaults object from app.html
 * (lines ~891-920). Persistence uses {@link SETTINGS_STORAGE_KEY}.
 */

import { SETTINGS_STORAGE_KEY } from "./storage.ts";

/** Theme preference stored in settings. */
export type ThemePreference = "auto" | "light" | "dark";

/**
 * Persisted user settings.
 *
 * Field names and semantics match the settings object in app.html. `provider`,
 * `voice`, and `model` hold select-option values (e.g. `"auto"`, `"default"`,
 * `"google:..."`, `"persona:..."`); they are free-form strings, not enums,
 * because the valid set depends on the live config.
 */
export interface WebSettings {
  provider: string;
  voice: string;
  model: string;
  theme: ThemePreference;
  emotionPreprocessing: boolean;
  summarization: boolean;
  generateOnPaste: boolean;
}

/**
 * Default settings.
 *
 * Ports the `defaults` object in `loadSettings` (app.html line ~892).
 */
export const DEFAULT_SETTINGS: WebSettings = {
  provider: "auto",
  voice: "default",
  model: "default",
  theme: "auto",
  emotionPreprocessing: true,
  summarization: false,
  generateOnPaste: true,
};

/**
 * Merge stored/partial settings onto the defaults.
 *
 * Ports the spread in `loadSettings` (`{ ...defaults, ...parsed }`, app.html
 * line ~902): keys present in `raw` override defaults, missing keys fall back.
 * No per-field validation is performed, matching the legacy tolerance.
 */
export function sanitizeSettings(raw: unknown): WebSettings {
  if (!raw || typeof raw !== "object") return { ...DEFAULT_SETTINGS };
  return { ...DEFAULT_SETTINGS, ...(raw as Partial<WebSettings>) };
}

/**
 * Load settings from localStorage, falling back to defaults.
 *
 * Ports `loadSettings` (app.html line ~891): parses the stored JSON (or `{}`)
 * and merges onto defaults; on parse failure returns a fresh defaults object.
 */
export function loadSettings(): WebSettings {
  try {
    return sanitizeSettings(JSON.parse(localStorage.getItem(SETTINGS_STORAGE_KEY) || "{}"));
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

/**
 * Persist settings to localStorage.
 *
 * Ports the `localStorage.setItem(settingsStorageKey, JSON.stringify(settings))`
 * write in `saveSettings` (app.html line ~918). Applying the theme side-effect
 * is left to the caller (B2); this helper only persists.
 */
export function saveSettings(settings: WebSettings): void {
  localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify(settings));
}
