/**
 * Prep-request transport: the Codex LLM client and the Google generateContent
 * client used by the speech-prep pipeline.
 *
 * Ports from app.html (lines ~2379-2553): `speechPrepModels`,
 * `speechPrepAttemptTimeoutMs`, `speechPrepErrorIsRetryable`, `base64UrlJson`,
 * `codexAccessTokenNeedsRefresh`, `refreshCodexAuth`, `ensureCodexAuth`,
 * `codexResponseBody`, `parseCodexSse`, `nonRetryableError`,
 * `fetchCodexPrepAttempt`, `fetchGooglePrepAttempt`, `fetchSpeechPrepAttempt`,
 * and `extractTextOutput`.
 *
 * Request shapes, headers, and SSE parsing are byte-for-byte faithful to the
 * live API calls.
 */

import type { BrowserCodexAuth } from "../config.ts";
import { normalizeGoogleModelName } from "../synth/google.ts";
import { DEFAULT_SPEECH_PREP_ATTEMPT_TIMEOUT_MS } from "./prompts.ts";
import type { EffectiveSpeechPrep } from "./types.ts";

/** An error whose retry disposition/status the prep loop consults. */
export interface PrepError extends Error {
  status?: number;
  retryable?: boolean;
}

/** Enumerate the distinct prep models (default + fallbacks). Ports `speechPrepModels`. */
export function speechPrepModels(prep: EffectiveSpeechPrep): string[] {
  const seen = new Set<string>();
  const models: string[] = [];
  for (const model of [prep.model, ...(prep.fallbackModels || [])]) {
    const normalized =
      prep.provider === "google"
        ? normalizeGoogleModelName(model)
        : String(model || "").replace(/^codex\//, "");
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    models.push(model);
  }
  return models;
}

/** Per-attempt timeout, bounded by the overall budget. Ports `speechPrepAttemptTimeoutMs`. */
export function speechPrepAttemptTimeoutMs(prep: EffectiveSpeechPrep): number {
  const configured = Number(prep.attemptTimeoutMs) || DEFAULT_SPEECH_PREP_ATTEMPT_TIMEOUT_MS;
  const overall = Number(prep.timeoutMs) || 30000;
  return Math.max(250, Math.min(configured, overall));
}

/** Whether a prep error should be retried. Ports `speechPrepErrorIsRetryable`. */
export function speechPrepErrorIsRetryable(error: PrepError | null | undefined): boolean {
  if (error?.status) return error.status === 429 || error.status >= 500;
  return error?.name === "AbortError" || error?.name === "TypeError";
}

/** Create a non-retryable error. Ports `nonRetryableError` (app.html line ~2505). */
export function nonRetryableError(message: string): PrepError {
  const error = new Error(message) as PrepError;
  error.retryable = false;
  return error;
}

/** Decode a base64url JWT segment as JSON. Ports `base64UrlJson`. */
export function base64UrlJson(segment: string | null | undefined): { exp?: number } {
  const normalized = String(segment || "")
    .replace(/-/g, "+")
    .replace(/_/g, "/");
  const padded = normalized + "=".repeat((4 - (normalized.length % 4)) % 4);
  return JSON.parse(atob(padded));
}

/** Whether a cached Codex access token needs a refresh. Ports `codexAccessTokenNeedsRefresh`. */
export function codexAccessTokenNeedsRefresh(auth: BrowserCodexAuth | null | undefined): boolean {
  try {
    const payload = base64UrlJson(String(auth?.accessToken || "").split(".")[1]);
    return (
      !Number.isFinite(payload.exp) ||
      (payload.exp as number) <= Math.floor(Date.now() / 1000) + 300
    );
  } catch {
    return true;
  }
}

async function prepProviderError(response: Response, fallback: string): Promise<PrepError> {
  let text = "";
  try {
    text = await response.text();
  } catch {
    // Ignored, matching app.html behavior.
  }
  const error = new Error(
    text ? `${fallback}: ${text}` : `${fallback} (${response.status})`,
  ) as PrepError;
  error.status = response.status;
  return error;
}

/**
 * Refresh the cached Codex OAuth token and re-cache it on `prep.codexAuth`.
 * Ports `refreshCodexAuth` (app.html line ~2425). The legacy persistence of
 * the mutated config to localStorage is delegated to `onRefreshed`.
 */
export async function refreshCodexAuth(
  prep: EffectiveSpeechPrep,
  onRefreshed?: (prep: EffectiveSpeechPrep) => void,
): Promise<BrowserCodexAuth> {
  const auth = prep.codexAuth;
  if (!auth?.refreshToken) throw new Error("Codex auth is missing a refresh token.");
  const response = await fetch(auth.tokenUrl || "https://auth.openai.com/oauth/token", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: auth.refreshToken,
      client_id: auth.clientId || "app_EMoamEEZ73f0CkXaXp7hrann",
    }),
  });
  if (!response.ok) throw await prepProviderError(response, "Codex auth refresh failed");
  const json = (await response.json()) as {
    access_token?: string;
    refresh_token?: string;
    account_id?: string;
  };
  prep.codexAuth = {
    ...auth,
    accessToken: json.access_token || auth.accessToken,
    refreshToken: json.refresh_token || auth.refreshToken,
    accountId: json.account_id || auth.accountId,
  };
  onRefreshed?.(prep);
  return prep.codexAuth;
}

/** Ensure a fresh Codex token, refreshing when near expiry. Ports `ensureCodexAuth`. */
export async function ensureCodexAuth(
  prep: EffectiveSpeechPrep,
  onRefreshed?: (prep: EffectiveSpeechPrep) => void,
): Promise<BrowserCodexAuth> {
  const auth = prep.codexAuth;
  if (!auth?.accessToken || !auth?.accountId) throw new Error("Codex auth is not cached.");
  if (!codexAccessTokenNeedsRefresh(auth)) return auth;
  return await refreshCodexAuth(prep, onRefreshed);
}

/** Build the Codex `/responses` request body. Ports `codexResponseBody`. */
export function codexResponseBody(
  prep: EffectiveSpeechPrep,
  model: string,
  prompt: string,
): Record<string, unknown> {
  const body: Record<string, unknown> = {
    model: String(model || "").replace(/^codex\//, ""),
    store: false,
    stream: true,
    instructions:
      "You are running non-interactively as a text transformation task. Do not use tools. Do not ask questions. Return only the transformed text.",
    input: [
      {
        type: "message",
        role: "user",
        content: [{ type: "input_text", text: prompt }],
      },
    ],
    text: { verbosity: "low" },
    parallel_tool_calls: false,
  };
  if (prep.reasoningEffort && prep.reasoningEffort !== "none") {
    body.reasoning = { effort: prep.reasoningEffort };
  }
  return body;
}

interface CodexSseEvent {
  type?: string;
  delta?: string;
  response?: {
    output_text?: string;
    output?: { type?: string; content?: { type?: string; text?: string }[] }[];
  };
}

/** Parse a Codex SSE response body into output text. Ports `parseCodexSse`. */
export function parseCodexSse(text: string | null | undefined): string {
  let outputText = "";
  let completed: CodexSseEvent["response"] | null = null;
  for (const line of String(text || "").split(/\r?\n/)) {
    if (!line.startsWith("data:")) continue;
    const data = line.slice(5).trim();
    if (!data || data === "[DONE]") continue;
    const event = JSON.parse(data) as CodexSseEvent;
    if (event.type === "response.output_text.delta" && typeof event.delta === "string") {
      outputText += event.delta;
    } else if (event.type === "response.completed" && event.response) {
      completed = event.response;
    } else if (event.type === "response.failed" || event.type === "response.incomplete") {
      throw new Error(`Codex prep ended with ${event.type}`);
    }
  }
  if (completed?.output_text) return completed.output_text;
  if (outputText) return outputText;
  const parts: string[] = [];
  for (const item of completed?.output || []) {
    if (item?.type !== "message") continue;
    for (const block of item.content || []) {
      if ((block.type === "output_text" || block.type === "text") && block.text)
        parts.push(block.text);
    }
  }
  return parts.join("");
}

/** POST a Codex prep attempt (with a 401/403 token refresh retry). Ports `fetchCodexPrepAttempt`. */
export async function fetchCodexPrepAttempt(
  prep: EffectiveSpeechPrep,
  model: string,
  prompt: string,
  signal: AbortSignal | null,
  onRefreshed?: (prep: EffectiveSpeechPrep) => void,
): Promise<Response> {
  const send = async (auth: BrowserCodexAuth): Promise<Response> =>
    fetch(
      `${String(prep.baseUrl || "")
        .replace(/\/$/, "")
        .replace(/\/responses$/, "")}/responses`,
      {
        method: "POST",
        signal,
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${auth.accessToken}`,
          "chatgpt-account-id": String(auth.accountId ?? ""),
          originator: "codex-voice-web",
          "User-Agent": "codex-voice-web",
          "OpenAI-Beta": "responses=experimental",
          Accept: "text/event-stream",
        },
        body: JSON.stringify(codexResponseBody(prep, model, prompt)),
      },
    );
  let response = await send(await ensureCodexAuth(prep, onRefreshed));
  if (response.status === 401 || response.status === 403) {
    response = await send(await refreshCodexAuth(prep, onRefreshed));
  }
  return response;
}

/** POST a Google generateContent prep attempt. Ports `fetchGooglePrepAttempt`. */
export async function fetchGooglePrepAttempt(
  prep: EffectiveSpeechPrep,
  model: string,
  body: unknown,
  signal: AbortSignal | null,
): Promise<Response> {
  return await fetch(
    `${prep.baseUrl}/models/${encodeURIComponent(normalizeGoogleModelName(model))}:generateContent`,
    {
      method: "POST",
      signal,
      headers: {
        "Content-Type": "application/json",
        "x-goog-api-key": prep.apiKey ?? "",
      },
      body: JSON.stringify(body),
    },
  );
}

/** Dispatch a prep attempt to the right transport. Ports `fetchSpeechPrepAttempt`. */
export async function fetchSpeechPrepAttempt(
  prep: EffectiveSpeechPrep,
  model: string,
  body: unknown,
  prompt: string,
  signal: AbortSignal | null,
  onRefreshed?: (prep: EffectiveSpeechPrep) => void,
): Promise<Response> {
  if (prep.provider === "codex") {
    return await fetchCodexPrepAttempt(prep, model, prompt, signal, onRefreshed);
  }
  return await fetchGooglePrepAttempt(prep, model, body, signal);
}

/** Extract concatenated text from a Google generateContent response. Ports `extractTextOutput`. */
export function extractTextOutput(json: unknown): string {
  const parts =
    (json as { candidates?: { content?: { parts?: { text?: string }[] } }[] })?.candidates?.[0]
      ?.content?.parts || [];
  return parts
    .map((part) => part.text || "")
    .filter(Boolean)
    .join(" ");
}

/** Milliseconds elapsed since a `performance.now()` mark. Ports `elapsedMs`. */
export function elapsedMs(startedAt: number): number {
  return Math.max(0, Math.round(performance.now() - startedAt));
}
