import { beforeEach, describe, expect, it } from "vitest";
import { DEFAULT_SETTINGS, loadSettings, sanitizeSettings, saveSettings } from "./settings.ts";
import { SETTINGS_STORAGE_KEY } from "./storage.ts";

beforeEach(() => {
  localStorage.clear();
});

describe("DEFAULT_SETTINGS", () => {
  it("matches the legacy defaults", () => {
    expect(DEFAULT_SETTINGS).toEqual({
      provider: "auto",
      voice: "default",
      model: "default",
      theme: "auto",
      emotionPreprocessing: true,
      summarization: false,
      generateOnPaste: true,
    });
  });
});

describe("sanitizeSettings", () => {
  it("merges partial values over defaults", () => {
    const result = sanitizeSettings({ provider: "google", summarization: true });
    expect(result.provider).toBe("google");
    expect(result.summarization).toBe(true);
    expect(result.voice).toBe("default"); // default preserved
  });

  it("returns fresh defaults for non-object input", () => {
    expect(sanitizeSettings(null)).toEqual(DEFAULT_SETTINGS);
    expect(sanitizeSettings("nope")).toEqual(DEFAULT_SETTINGS);
  });
});

describe("loadSettings / saveSettings", () => {
  it("returns defaults when nothing is stored", () => {
    expect(loadSettings()).toEqual(DEFAULT_SETTINGS);
  });

  it("round-trips saved settings", () => {
    const settings = { ...DEFAULT_SETTINGS, provider: "elevenlabs", theme: "dark" as const };
    saveSettings(settings);
    expect(loadSettings()).toEqual(settings);
  });

  it("fills missing keys from defaults", () => {
    localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify({ provider: "google" }));
    const loaded = loadSettings();
    expect(loaded.provider).toBe("google");
    expect(loaded.generateOnPaste).toBe(true);
  });

  it("tolerates corrupt JSON by returning defaults", () => {
    localStorage.setItem(SETTINGS_STORAGE_KEY, "{broken");
    expect(loadSettings()).toEqual(DEFAULT_SETTINGS);
  });
});
