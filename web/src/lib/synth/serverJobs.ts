/**
 * Server-side speech job client (`/web/speech-jobs`).
 *
 * Ports `createWebSpeechJob`, `fetchWebSpeechJob`, and `waitForWebSpeechJob`
 * from app.html (lines ~3488-3543), plus the poll-interval and max-poll
 * constants. The UI never calls a synchronous `/web/speech` endpoint, so only
 * the job create/poll path is ported.
 *
 * Response shapes mirror the serde structs in
 * `crates/codex-voice-transcriber/src/server/web.rs`
 * (`WebSpeechJobStatusResponse`, `WebSpeechResponse`, `WebSpeechJobError`).
 */

/** Poll interval between job status checks. Ports `serverJobPollMs`. */
export const SERVER_JOB_POLL_MS = 1200;

/** Max time to keep polling before giving up. Ports `serverJobMaxPollMs` (10m). */
export const SERVER_JOB_MAX_POLL_MS = 10 * 60 * 1000;

/** Completed job payload (`WebSpeechResponse`). */
export interface WebSpeechResult {
  input: string;
  input_changed: boolean;
  audio_base64: string;
  mime_type: string;
  format: string;
}

/** Job error payload (`WebSpeechJobError`). */
export interface WebSpeechJobError {
  status: number;
  kind: string;
  message: string;
}

/** Job status response (`WebSpeechJobStatusResponse`). */
export interface WebSpeechJobStatus {
  id: string;
  status: "pending" | "complete" | "failed";
  phase?: "queued" | "running";
  result?: WebSpeechResult;
  error?: WebSpeechJobError;
}

/** Optional server-side provider and prep overrides for a generation. */
export interface WebSpeechJobOptions {
  provider?: string;
  voice?: string;
  model?: string;
  speechPrepEnabled?: boolean;
}

/** Cancel a queued/running job or release a completed server result. */
export async function cancelWebSpeechJob(jobId: string): Promise<void> {
  const response = await fetch(`/web/speech-jobs/${encodeURIComponent(jobId)}`, {
    method: "DELETE",
    keepalive: true,
  });
  if (!response.ok && response.status !== 404) {
    throw new Error(`TTS job cancellation failed (${response.status})`);
  }
}

/** Options for {@link waitForWebSpeechJob}. */
export interface WaitForJobOptions {
  signal?: AbortSignal | null;
  /** Cancellation check invoked each poll iteration (throws to abort). */
  throwIfCancelled?: () => void;
  /**
   * Optional progress callback, invoked with the same values the legacy UI
   * used: `(0.82, 'Loading')` once before polling and `(0.64, 'Synthesizing')`
   * after each non-terminal poll.
   */
  onProgress?: (value: number, label: string) => void;
}

/** Resolve after `ms` milliseconds. Ports the `sleep` helper (app.html line ~3490). */
export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Create a server speech job and return its id.
 *
 * POSTs the input plus optional provider/prep overrides to
 * `/web/speech-jobs`. On a non-OK response, throws with the server-provided
 * `error.message` when parseable, else `"TTS job failed ({status})"`. Throws
 * when the response lacks an `id`.
 */
export async function createWebSpeechJob(
  input: string,
  signal: AbortSignal | null = null,
  options: WebSpeechJobOptions = {},
): Promise<string> {
  const response = await fetch("/web/speech-jobs", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    signal,
    body: JSON.stringify({ input, ...options }),
  });
  if (!response.ok) {
    let message = `TTS job failed (${response.status})`;
    try {
      const json = (await response.json()) as { error?: { message?: string } };
      message = json?.error?.message || message;
    } catch {
      // Ignored, matching app.html behavior.
    }
    throw new Error(message);
  }
  const job = (await response.json()) as { id?: string };
  if (!job?.id) throw new Error("TTS job did not return an id.");
  return job.id;
}

/**
 * Fetch a single job status snapshot.
 *
 * Ports `fetchWebSpeechJob` (app.html line ~3514): GETs
 * `/web/speech-jobs/{id}` with `cache: 'no-store'`. On a non-OK response,
 * throws with the server `error.message` when parseable, else
 * `"TTS job status failed ({status})"`.
 */
export async function fetchWebSpeechJob(
  jobId: string,
  signal: AbortSignal | null = null,
): Promise<WebSpeechJobStatus> {
  const response = await fetch(`/web/speech-jobs/${encodeURIComponent(jobId)}`, {
    cache: "no-store",
    signal,
  });
  if (!response.ok) {
    let message = `TTS job status failed (${response.status})`;
    try {
      const json = (await response.json()) as { error?: { message?: string } };
      message = json?.error?.message || message;
    } catch {
      // Ignored, matching app.html behavior.
    }
    throw new Error(message);
  }
  return (await response.json()) as WebSpeechJobStatus;
}

/** Error carrying an HTTP-ish status, thrown by {@link waitForWebSpeechJob}. */
export class ServerJobError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "ServerJobError";
    this.status = status;
  }
}

/**
 * Poll a job until it completes, fails, or times out.
 *
 * Ports `waitForWebSpeechJob` (app.html line ~3527): polls every
 * {@link SERVER_JOB_POLL_MS} until the job is `complete` (returns its result),
 * `failed` (throws with `error.message` or `"TTS job failed."`), or the elapsed
 * time exceeds {@link SERVER_JOB_MAX_POLL_MS} — in which case it throws a
 * {@link ServerJobError} with status 408 and the exact legacy message. The
 * cancellation check runs at the top of each iteration.
 */
export async function waitForWebSpeechJob(
  jobId: string,
  options: WaitForJobOptions = {},
): Promise<WebSpeechResult> {
  const { signal = null, throwIfCancelled, onProgress } = options;
  const startedAt = Date.now();
  onProgress?.(0.82, "Loading");
  for (;;) {
    throwIfCancelled?.();
    if (Date.now() - startedAt > SERVER_JOB_MAX_POLL_MS) {
      throw new ServerJobError(
        "TTS job stayed pending for too long. It was cleared so you can generate again.",
        408,
      );
    }
    const job = await fetchWebSpeechJob(jobId, signal);
    if (job.status === "complete" && job.result) return job.result;
    if (job.status === "failed") throw new Error(job.error?.message || "TTS job failed.");
    onProgress?.(
      job.phase === "queued" ? 0.48 : 0.64,
      job.phase === "queued" ? "Waiting" : "Synthesizing",
    );
    await sleep(SERVER_JOB_POLL_MS);
  }
}
