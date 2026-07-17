const DEFAULT_PROVIDER_TIMEOUT_MS = 30_000;
const LONG_TTS_TIMEOUT_MAX_MS = 300_000;

/** Match Rust's `tts_timeout_for_input` scaling for one provider request. */
export function ttsTimeoutForInput(baseTimeoutMs: number, input: string): number {
  const base = Number(baseTimeoutMs) || DEFAULT_PROVIDER_TIMEOUT_MS;
  const chars = Array.from(input).length;
  if (chars <= 1_200) return base;

  const scaled = Math.max(90_000, Math.min(Math.floor(chars / 25) * 1_000, 300_000));
  return Math.min(Math.max(base, scaled), LONG_TTS_TIMEOUT_MAX_MS);
}

export interface ProviderTimeoutSignal {
  signal: AbortSignal;
  dispose: () => void;
}

/** Combine user cancellation with the provider's input-scaled timeout. */
export function providerTimeoutSignal(
  baseTimeoutMs: number,
  input: string,
  externalSignal: AbortSignal | null | undefined,
): ProviderTimeoutSignal {
  const controller = new AbortController();
  const abortFromExternal = (): void => controller.abort(externalSignal?.reason);
  if (externalSignal?.aborted) abortFromExternal();
  else externalSignal?.addEventListener("abort", abortFromExternal, { once: true });

  const timer = setTimeout(
    () => {
      const error = new Error("TTS provider timed out.");
      error.name = "TimeoutError";
      controller.abort(error);
    },
    ttsTimeoutForInput(baseTimeoutMs, input),
  );

  return {
    signal: controller.signal,
    dispose: () => {
      clearTimeout(timer);
      externalSignal?.removeEventListener("abort", abortFromExternal);
    },
  };
}
