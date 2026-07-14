/**
 * Desktop-app-webview URL contract.
 *
 * Tauri windows load this PWA over HTTP as dumb webviews (no Tauri IPC, no
 * `window.__TAURI__`); the Rust side signals app-mode context purely through
 * the URL: query params for window flags, the fragment for one-shot payload
 * data.
 *
 * - `?app=1` — the window is a desktop app webview ("app mode"); the
 *   PWA-specific behaviors (service-worker registration) are skipped.
 * - `?view=settings` — a settings-only window: the settings drawer starts
 *   open.
 * - `#intent=<128-bit hex id>` — consume selected text from the local service,
 *   prefill the editor, and auto-start generation once.
 *
 * All helpers here are pure (take the `search`/`hash` string explicitly)
 * so they are unit-testable without touching `location`/`window`.
 */

/** True when the `app=1` query param is present (desktop app webview). */
export function isAppMode(search: string): boolean {
  return new URLSearchParams(search).get("app") === "1";
}

/** True when `view=settings` is present (settings-only window). */
export function settingsView(search: string): boolean {
  return new URLSearchParams(search).get("view") === "settings";
}

/** Parse and validate a one-shot desktop intent id from the URL fragment. */
export function speakIntentId(hash: string): string | null {
  const trimmed = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!trimmed) return null;
  const params = new URLSearchParams(trimmed);
  const id = params.get("intent");
  return id && /^[a-f0-9]{32}$/i.test(id) ? id.toLowerCase() : null;
}

/** Consume a one-shot selected-text intent from the local service. */
export async function consumeDesktopIntent(id: string): Promise<string> {
  const response = await fetch(`/web/desktop-intents/${encodeURIComponent(id)}`, {
    cache: "no-store",
  });
  if (!response.ok) {
    let message = `Selected text handoff failed (${response.status}).`;
    try {
      const body = (await response.json()) as { error?: { message?: string } };
      message = body.error?.message || message;
    } catch {
      // Keep the status-based fallback.
    }
    throw new Error(message);
  }
  const body = (await response.json()) as { text?: unknown };
  if (typeof body.text !== "string" || !body.text.trim()) {
    throw new Error("Selected text handoff returned no text.");
  }
  return body.text;
}
