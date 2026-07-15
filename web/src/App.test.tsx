import { fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { App } from "./App.tsx";
import { markWorkerUpdateNotice } from "./pwa.ts";

// The mount effect fetches /web/config and reads IndexedDB. Stub fetch to an
// offline reject (fetchConfig swallows it and returns null) so tests exercise
// the shell without a backend.
beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("offline"))),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  delete document.documentElement.dataset.theme;
  // Restore the URL even when a test failed mid-assertion, so a leftover
  // ?view=settings or #intent= fragment can't leak into the next render.
  window.history.pushState(null, "", "/web");
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

test("shows the update toast only after a worker-triggered reload", () => {
  const first = render(<App />);
  expect(document.getElementById("update-toast")).toBeNull();
  first.unmount();

  markWorkerUpdateNotice();
  render(<App />);

  expect(document.getElementById("update-toast")?.textContent).toContain(
    "Updated to latest version",
  );
  expect(sessionStorage.length).toBe(0);
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

test("generate with empty text surfaces the error banner", () => {
  render(<App />);
  const generate = document.getElementById("generate") as HTMLButtonElement;
  const banner = document.getElementById("error-banner") as HTMLElement;
  expect(banner.textContent).toBe("");
  expect(banner.classList.contains("hidden")).toBe(true);

  fireEvent.click(generate);
  expect(banner.textContent).toBe("Enter some text first.");
  expect(banner.classList.contains("flex")).toBe(true);
  expect(banner.classList.contains("hidden")).toBe(false);
});

test("generate button remains enabled and cancels an active generation", async () => {
  let generationSignal: AbortSignal | undefined;
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === "/web/config") return Promise.reject(new Error("offline"));
      if (String(input) === "/web/speech-jobs" && init?.method === "POST") {
        generationSignal = init.signal ?? undefined;
        return new Promise<Response>((_resolve, reject) => {
          generationSignal?.addEventListener("abort", () => {
            reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
          });
        });
      }
      return Promise.reject(new Error("offline"));
    }),
  );

  render(<App />);
  const text = document.getElementById("text") as HTMLTextAreaElement;
  const generate = document.getElementById("generate") as HTMLButtonElement;
  const label = document.getElementById("generate-label") as HTMLElement;
  fireEvent.input(text, { target: { value: "Cancel this generation" } });
  fireEvent.click(generate);

  await waitFor(() => expect(generationSignal).toBeDefined());
  expect(generate.disabled).toBe(false);
  expect(label.children[0]?.textContent).toBe("Generating...");
  expect(label.children[1]?.textContent).toBe("Tap to Stop");

  fireEvent.click(generate);

  expect(generationSignal?.aborted).toBe(true);
  await waitFor(() => expect(label.textContent).toBe("Generate"));
});

test("toggling a settings checkbox persists to localStorage", () => {
  render(<App />);
  const emotion = document.getElementById("emotion") as HTMLInputElement;
  // Default is on; toggling it off must persist.
  expect(emotion.checked).toBe(true);

  fireEvent.click(emotion);
  expect(emotion.checked).toBe(false);
  const raw = localStorage.getItem("codex-voice.web.settings.v1");
  expect(raw).not.toBeNull();
  expect(JSON.parse(raw as string).emotionPreprocessing).toBe(false);
});

test("?view=settings renders only the settings surface", () => {
  window.history.pushState(null, "", "/web?view=settings");
  render(<App />);
  const panel = document.getElementById("settings-panel") as HTMLElement;
  expect(panel.hasAttribute("hidden")).toBe(false);
  expect(document.getElementById("text")).toBeNull();
  expect(document.getElementById("generate")).toBeNull();
  expect(document.getElementById("settings-toggle")).toBeNull();
  expect((document.getElementById("provider") as HTMLSelectElement).disabled).toBe(false);
  expect((document.getElementById("emotion") as HTMLInputElement).disabled).toBe(false);
});

test("#intent= intake consumes text, clears the hash, and fires generation once", async () => {
  const sample = "spoken by the desktop app 🎙️";
  const intentId = "a".repeat(32);
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes(`/web/desktop-intents/${intentId}`)) {
      return new Response(JSON.stringify({ text: sample }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    throw new Error("offline");
  });
  vi.stubGlobal("fetch", fetchMock);
  window.history.pushState(null, "", `/web#intent=${intentId}`);

  render(<App />);
  const text = document.getElementById("text") as HTMLTextAreaElement;

  await waitFor(() => expect(text.value).toBe(sample));
  await waitFor(() => expect(location.hash).toBe(""));

  // A generation attempt actually fired the server-job pipeline (rather than
  // just seeding the text): the request hits /web/speech-jobs. The fetch stub
  // rejects, which the pipeline turns into an error-banner message.
  const speechJobCalls = (): number =>
    (global.fetch as ReturnType<typeof vi.fn>).mock.calls.filter((call) =>
      String(call[0]).includes("/web/speech-jobs"),
    ).length;
  await waitFor(() => expect(speechJobCalls()).toBeGreaterThan(0));
  // Drain any second deferred generate (setTimeout -> dynamic import -> fetch)
  // before asserting once-ness, so a double-firing intake fails deterministically
  // instead of racing the count.
  await new Promise((resolve) => setTimeout(resolve, 25));
  await new Promise((resolve) => setTimeout(resolve, 25));
  expect(speechJobCalls()).toBe(1);
});

test("a slower older desktop intent cannot replace the newest selection", async () => {
  const firstId = "1".repeat(32);
  const secondId = "2".repeat(32);
  let resolveFirst!: (response: Response) => void;
  let resolveSecond!: (response: Response) => void;
  const first = new Promise<Response>((resolve) => {
    resolveFirst = resolve;
  });
  const second = new Promise<Response>((resolve) => {
    resolveSecond = resolve;
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes(`/web/desktop-intents/${firstId}`)) return await first;
      if (url.includes(`/web/desktop-intents/${secondId}`)) return await second;
      throw new Error("offline");
    }),
  );
  render(<App />);
  const text = document.getElementById("text") as HTMLTextAreaElement;

  window.history.pushState(null, "", `/web#intent=${firstId}`);
  window.dispatchEvent(new HashChangeEvent("hashchange"));
  window.history.pushState(null, "", `/web#intent=${secondId}`);
  window.dispatchEvent(new HashChangeEvent("hashchange"));

  resolveSecond(
    new Response(JSON.stringify({ text: "newest selection" }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    }),
  );
  await waitFor(() => expect(text.value).toBe("newest selection"));

  resolveFirst(
    new Response(JSON.stringify({ text: "stale selection" }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 25));
  expect(text.value).toBe("newest selection");
});

test("failed #intent= intake clears the hash and surfaces the error", async () => {
  const intentId = "b".repeat(32);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      if (String(input).includes(`/web/desktop-intents/${intentId}`)) {
        return new Response(JSON.stringify({ error: { message: "intent expired" } }), {
          status: 404,
          headers: { "Content-Type": "application/json" },
        });
      }
      throw new Error("offline");
    }),
  );
  window.history.pushState(null, "", `/web#intent=${intentId}`);

  render(<App />);

  await waitFor(() => expect(location.hash).toBe(""));
  await waitFor(() =>
    expect(document.getElementById("error-banner")?.textContent).toBe("intent expired"),
  );
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

test("an empty clipboard paste is a complete no-op", async () => {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { readText: vi.fn(() => Promise.resolve("")) },
  });
  localStorage.setItem("codex-voice.web.text", "keep this draft");

  render(<App />);
  const paste = document.getElementById("paste") as HTMLButtonElement;
  const text = document.getElementById("text") as HTMLTextAreaElement;
  fireEvent.click(paste);

  await waitFor(() => expect(navigator.clipboard.readText).toHaveBeenCalledOnce());
  expect(text.value).toBe("keep this draft");
  expect(localStorage.getItem("codex-voice.web.text")).toBe("keep this draft");
});

test("consecutive clipboard-button pastes generate the newly pasted text", async () => {
  const clipboard = { readText: vi.fn<() => Promise<string>>() };
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
  const generated: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === "/web/config") throw new Error("offline");
      if (String(input) === "/web/speech-jobs" && init?.method === "POST") {
        generated.push((JSON.parse(String(init.body)) as { input: string }).input);
      }
      throw new Error("offline");
    }),
  );

  render(<App />);
  const paste = document.getElementById("paste") as HTMLButtonElement;
  const text = document.getElementById("text") as HTMLTextAreaElement;

  for (const value of ["first pasted draft", "second pasted draft"]) {
    clipboard.readText.mockResolvedValueOnce(value);
    fireEvent.click(paste);
    await waitFor(() => expect(text.value).toBe(value));
    await waitFor(() => expect(generated.at(-1)).toBe(value));
  }

  expect(generated).toEqual(["first pasted draft", "second pasted draft"]);
});
