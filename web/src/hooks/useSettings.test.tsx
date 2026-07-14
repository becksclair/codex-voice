import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { SETTINGS_STORAGE_KEY } from "../lib/storage.ts";
import { useSettings } from "./useSettings.ts";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  delete document.documentElement.dataset.theme;
  vi.restoreAllMocks();
});

// happy-dom (like real browsers) does not fire `storage` in the same window
// that made the write, so cross-window sync is exercised by dispatching the
// event manually, as another window's `storage` listener would receive it.
function dispatchStorage(newValue: string): void {
  window.dispatchEvent(
    new StorageEvent("storage", {
      key: SETTINGS_STORAGE_KEY,
      newValue,
      storageArea: localStorage,
    }),
  );
}

test("a storage event from another window reloads settings into state", () => {
  const { result } = renderHook(() => useSettings(null));
  expect(result.current.settings.theme).toBe("auto");

  const external = { ...result.current.settings, theme: "light" };
  localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify(external));
  act(() => dispatchStorage(JSON.stringify(external)));

  expect(result.current.settings.theme).toBe("light");
});

test("a window focus event reloads settings (WebKitGTK storage-event fallback)", () => {
  const { result } = renderHook(() => useSettings(null));
  expect(result.current.settings.summarization).toBe(false);

  const external = { ...result.current.settings, summarization: true };
  localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify(external));
  act(() => window.dispatchEvent(new Event("focus")));

  expect(result.current.settings.summarization).toBe(true);
});

test("a null config leaves a stored provider selection intact (no clamp/persist)", () => {
  // A prior session that had config loaded persisted provider: "google".
  localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify({ provider: "google" }));

  const { result } = renderHook(() => useSettings(null));

  // With no config yet, the real selection must survive in state...
  expect(result.current.settings.provider).toBe("google");
  // ...and must not have been clamped down to "auto" and persisted, which
  // cross-window sync would then push into a sibling window that did load config.
  const persisted = JSON.parse(localStorage.getItem(SETTINGS_STORAGE_KEY) as string);
  expect(persisted.provider).toBe("google");
});

test("applying an identical external value does not re-persist", () => {
  const { result } = renderHook(() => useSettings(null));
  const setItemSpy = vi.spyOn(Storage.prototype, "setItem");
  setItemSpy.mockClear();

  // The value already on disk matches in-memory state exactly.
  localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify(result.current.settings));
  setItemSpy.mockClear();

  act(() => dispatchStorage(JSON.stringify(result.current.settings)));
  act(() => window.dispatchEvent(new Event("focus")));

  expect(setItemSpy).not.toHaveBeenCalled();
});
