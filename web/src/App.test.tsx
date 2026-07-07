import { fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { App } from "./App.tsx";

// The mount effect fetches /web/config and reads IndexedDB. Stub fetch to an
// offline reject (fetchConfig swallows it and returns null) so tests exercise
// the shell without a backend.
beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("offline"))),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  delete document.documentElement.dataset.theme;
});

// The frozen test contract: every element ID the Playwright suite and the Rust
// regression guards depend on must exist in the DOM.
const REQUIRED_IDS = [
  "clear",
  "count",
  "download",
  "duration",
  "elapsed",
  "emotion",
  "error-banner",
  "generate",
  "generate-label",
  "generate-on-paste",
  "model",
  "paste",
  "play",
  "play-icon",
  "provider",
  "settings-panel",
  "settings-toggle",
  "summarize",
  "text",
  "theme",
  "voice",
  "waveform",
  "waveform-slider",
];

test("renders every element ID in the frozen test contract", () => {
  render(<App />);
  for (const id of REQUIRED_IDS) {
    expect(document.getElementById(id), `missing #${id}`).not.toBeNull();
  }
});

test("character count updates as the user types", () => {
  render(<App />);
  const text = document.getElementById("text") as HTMLTextAreaElement;
  const count = document.getElementById("count") as HTMLElement;
  expect(count.textContent).toBe("0 chars");

  fireEvent.input(text, { target: { value: "a" } });
  expect(count.textContent).toBe("1 char");

  fireEvent.input(text, { target: { value: "abcd" } });
  expect(count.textContent).toBe("4 chars");
});

test("settings toggle shows and hides the panel", () => {
  render(<App />);
  const toggle = document.getElementById("settings-toggle") as HTMLButtonElement;
  const panel = document.getElementById("settings-panel") as HTMLElement;
  expect(panel.hasAttribute("hidden")).toBe(true);
  expect(toggle.getAttribute("aria-expanded")).toBe("false");

  fireEvent.click(toggle);
  expect(panel.hasAttribute("hidden")).toBe(false);
  expect(toggle.getAttribute("aria-expanded")).toBe("true");

  fireEvent.click(toggle);
  expect(panel.hasAttribute("hidden")).toBe(true);
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
});

test("theme select persists to localStorage and sets data-theme", () => {
  render(<App />);
  const theme = document.getElementById("theme") as HTMLSelectElement;
  fireEvent.change(theme, { target: { value: "light" } });

  expect(document.documentElement.dataset.theme).toBe("light");
  const raw = localStorage.getItem("codex-voice.web.settings.v1");
  expect(raw).not.toBeNull();
  expect(JSON.parse(raw as string).theme).toBe("light");
});

test("paste fills the textarea without moving focus", async () => {
  const pasted = "clipboard payload";
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { readText: vi.fn(() => Promise.resolve(pasted)) },
  });

  render(<App />);
  const text = document.getElementById("text") as HTMLTextAreaElement;
  const paste = document.getElementById("paste") as HTMLButtonElement;
  const generateOnPaste = document.getElementById("generate-on-paste") as HTMLInputElement;
  const settingsToggle = document.getElementById("settings-toggle") as HTMLButtonElement;

  // Disable generate-on-paste so the paste flow does not attempt generation.
  fireEvent.click(generateOnPaste);
  expect(generateOnPaste.checked).toBe(false);

  // Move focus off the textarea, then paste.
  settingsToggle.focus();
  expect(document.activeElement).toBe(settingsToggle);

  fireEvent.click(paste);
  await waitFor(() => expect(text.value).toBe(pasted));

  // The regression guard: paste must NOT refocus the textarea.
  expect(document.activeElement).not.toBe(text);
});
