/**
 * Shared helpers for the browser-direct provider clients.
 *
 * Ports `providerError` (app.html line ~2379) and `selectedProviderModel`
 * (line ~1887).
 */

/**
 * An error carrying an HTTP `status`, as thrown by the provider clients.
 *
 * The legacy code sets a `.status` property on a plain `Error`; this subclass
 * makes that contract explicit and type-safe. `retryable` mirrors the optional
 * flag the legacy `isRetryable` consults (`error?.retryable === false`).
 */
export class ProviderError extends Error {
  status: number;
  retryable?: boolean;

  constructor(message: string, status: number) {
    super(message);
    this.name = "ProviderError";
    this.status = status;
  }
}

/**
 * Build a {@link ProviderError} from a failed `Response`.
 *
 * Ports `providerError` (app.html line ~2379): reads the response body as text
 * (ignoring read failures) and formats the message as `"{fallback}: {body}"`
 * when a body is present, else `"{fallback} ({status})"`. The `status` is
 * attached to the error.
 */
export async function providerError(response: Response, fallback: string): Promise<ProviderError> {
  let text = "";
  try {
    text = await response.text();
  } catch {
    // Ignored, matching app.html behavior.
  }
  const message = text ? `${fallback}: ${text}` : `${fallback} (${response.status})`;
  return new ProviderError(message, response.status);
}

/**
 * Resolve the model to use for a provider from the settings `model` value.
 *
 * Ports `selectedProviderModel` (app.html line ~1887): when `settingsModel`
 * starts with `"{provider}:"`, the suffix is the chosen model; otherwise the
 * provider's `defaultModel` is used. Passing `undefined`/`null` for
 * `settingsModel` (or the sentinel `"default"`) yields the default.
 */
export function selectedProviderModel(
  settingsModel: string | null | undefined,
  provider: string,
  defaultModel: string,
): string {
  const prefix = `${provider}:`;
  return settingsModel?.startsWith(prefix) ? settingsModel.slice(prefix.length) : defaultModel;
}
