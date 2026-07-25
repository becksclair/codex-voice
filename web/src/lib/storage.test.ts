import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { clearLegacyPersistentState, SETTINGS_STORAGE_KEY, TEXT_STORAGE_KEY } from "./storage.ts";

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

afterEach(() => vi.unstubAllGlobals());

test("legacy offline state is removed while draft and settings survive", () => {
  const deleteDatabase = vi.fn();
  vi.stubGlobal("indexedDB", { deleteDatabase });
  localStorage.setItem(TEXT_STORAGE_KEY, "draft");
  localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify({ theme: "dark" }));
  localStorage.setItem("codex-voice.web.config.v1", "secret-bearing config");
  localStorage.setItem("codex-voice.web.generation.v1", "pending job");
  sessionStorage.setItem("codex-voice.web.worker-update-notice", "1");
  sessionStorage.setItem("codex-voice.web.app-mode-worker-cleanup", "1");

  clearLegacyPersistentState();

  expect(localStorage.getItem(TEXT_STORAGE_KEY)).toBe("draft");
  expect(localStorage.getItem(SETTINGS_STORAGE_KEY)).toContain("dark");
  expect(localStorage.getItem("codex-voice.web.config.v1")).toBeNull();
  expect(localStorage.getItem("codex-voice.web.generation.v1")).toBeNull();
  expect(sessionStorage.getItem("codex-voice.web.worker-update-notice")).toBeNull();
  expect(sessionStorage.getItem("codex-voice.web.app-mode-worker-cleanup")).toBeNull();
  expect(deleteDatabase).toHaveBeenCalledWith("codex-voice-web-audio");
});
