/** The only durable browser state: draft text and ordinary user settings. */
export const TEXT_STORAGE_KEY = "codex-voice.web.text";
export const SETTINGS_STORAGE_KEY = "codex-voice.web.settings.v1";
const LEGACY_LOCAL_STORAGE_KEYS = [
  "codex-voice.web.config.v1",
  "codex-voice.web.generation.v1",
] as const;
const LEGACY_SESSION_STORAGE_KEYS = [
  "codex-voice.web.worker-update-notice",
  "codex-voice.web.app-mode-worker-cleanup",
] as const;
const LEGACY_AUDIO_DATABASE = "codex-voice-web-audio";

/** Remove durable state from the retired offline/browser-direct implementation. */
export function clearLegacyPersistentState(): void {
  for (const key of LEGACY_LOCAL_STORAGE_KEYS) localStorage.removeItem(key);
  for (const key of LEGACY_SESSION_STORAGE_KEYS) sessionStorage.removeItem(key);
  if (typeof indexedDB !== "undefined") indexedDB.deleteDatabase(LEGACY_AUDIO_DATABASE);
}

export function loadText(): string {
  return localStorage.getItem(TEXT_STORAGE_KEY) || "";
}

export function saveText(value: string): void {
  localStorage.setItem(TEXT_STORAGE_KEY, value);
}
