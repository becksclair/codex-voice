/**
 * Storage keys and typed persistence helpers.
 *
 * Ports the localStorage/IndexedDB helpers from app.html:
 * - key constants (lines ~768-774)
 * - draft-text load/save (`text.value = localStorage.getItem(...)`, line ~816)
 * - pending-generation state (`savePendingGeneration`/`loadPendingGeneration`/
 *   `clearPendingGeneration`, lines ~1735-1764)
 * - the generated-audio IndexedDB store (lines ~1685-1733, 1802-1816)
 *
 * All string keys, the DB/store/record names, and record shapes are preserved
 * exactly so this coexists with data written by the legacy PWA.
 */

/** localStorage key for the draft text. */
export const TEXT_STORAGE_KEY = "codex-voice.web.text";
/** localStorage key for the cached `/web/config` payload. */
export const CONFIG_STORAGE_KEY = "codex-voice.web.config.v1";
/** localStorage key for user settings. */
export const SETTINGS_STORAGE_KEY = "codex-voice.web.settings.v1";
/** localStorage key for in-flight generation state. */
export const GENERATION_STATE_STORAGE_KEY = "codex-voice.web.generation.v1";

/** IndexedDB database name for generated audio. */
export const GENERATED_AUDIO_DB_NAME = "codex-voice-web-audio";
/** IndexedDB object-store name for generated audio. */
export const GENERATED_AUDIO_STORE = "generated";
/** Keypath value of the single "last generated" audio record. */
export const LAST_GENERATED_AUDIO_KEY = "last";

/** Pending generations older than this (6h) are discarded on load. */
export const MAX_PENDING_GENERATION_AGE_MS = 6 * 60 * 60 * 1000;

/**
 * Load the persisted draft text.
 *
 * Ports `localStorage.getItem(textStorageKey) || ''` (app.html line ~816).
 */
export function loadText(): string {
  return localStorage.getItem(TEXT_STORAGE_KEY) || "";
}

/**
 * Persist the draft text.
 *
 * Ports the repeated `localStorage.setItem(textStorageKey, ...)` writes in
 * app.html (e.g. lines ~1809, 3599, 3675).
 */
export function saveText(value: string): void {
  localStorage.setItem(TEXT_STORAGE_KEY, value);
}

/** In-flight generation state persisted across reloads. */
export interface PendingGeneration {
  input: string;
  jobId: string | null;
  startedAt: number;
}

/**
 * Persist in-flight generation state.
 *
 * Ports `savePendingGeneration` (app.html line ~1735). `startedAt` is stamped
 * with `Date.now()` at call time.
 */
export function savePendingGeneration(input: string, jobId: string | null = null): void {
  localStorage.setItem(
    GENERATION_STATE_STORAGE_KEY,
    JSON.stringify({ input, jobId, startedAt: Date.now() }),
  );
}

/**
 * Load resumable generation state, or `null` if there is nothing to resume.
 *
 * Ports `loadPendingGeneration` (app.html line ~1743). Returns `null` — and
 * clears the stored value — when the record is missing `input`/`startedAt`,
 * has no `jobId`, is older than {@link MAX_PENDING_GENERATION_AGE_MS}, or fails
 * to parse. Only server-job generations (those with a `jobId`) are resumable.
 */
export function loadPendingGeneration(): PendingGeneration | null {
  try {
    const pending = JSON.parse(
      localStorage.getItem(GENERATION_STATE_STORAGE_KEY) || "null",
    ) as PendingGeneration | null;
    if (!pending?.input || !pending?.startedAt) return null;
    if (!pending.jobId) {
      localStorage.removeItem(GENERATION_STATE_STORAGE_KEY);
      return null;
    }
    if (Date.now() - pending.startedAt > MAX_PENDING_GENERATION_AGE_MS) {
      localStorage.removeItem(GENERATION_STATE_STORAGE_KEY);
      return null;
    }
    return pending;
  } catch {
    localStorage.removeItem(GENERATION_STATE_STORAGE_KEY);
    return null;
  }
}

/**
 * Clear any persisted generation state.
 *
 * Ports `clearPendingGeneration` (app.html line ~1762).
 */
export function clearPendingGeneration(): void {
  localStorage.removeItem(GENERATION_STATE_STORAGE_KEY);
}

/**
 * Record shape stored in the generated-audio IndexedDB store.
 *
 * Matches the object written by `saveLastGeneratedAudio` (app.html line
 * ~1716). `id` is the store keypath.
 */
export interface GeneratedAudioRecord {
  id: string;
  text: string;
  blob: Blob;
  mimeType: string;
  inputChanged: boolean;
  createdAt: string;
}

/**
 * Open (and if needed, upgrade) the generated-audio IndexedDB database.
 *
 * Ports `openGeneratedAudioDb` (app.html line ~1685). Rejects if IndexedDB is
 * unavailable. The `generated` store uses keypath `id`.
 */
export function openGeneratedAudioDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (!("indexedDB" in globalThis)) {
      reject(new Error("IndexedDB is not available."));
      return;
    }
    const request = indexedDB.open(GENERATED_AUDIO_DB_NAME, 1);
    request.onupgradeneeded = () => {
      request.result.createObjectStore(GENERATED_AUDIO_STORE, { keyPath: "id" });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error("Could not open audio storage."));
  });
}

/**
 * Run a single request against the generated-audio store inside one
 * transaction, resolving with the request result.
 *
 * Ports `withGeneratedAudioStore` (app.html line ~1700), including closing the
 * database in a `finally` block.
 */
export async function withGeneratedAudioStore<T>(
  mode: IDBTransactionMode,
  callback: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await openGeneratedAudioDb();
  try {
    return await new Promise<T>((resolve, reject) => {
      const transaction = db.transaction(GENERATED_AUDIO_STORE, mode);
      const store = transaction.objectStore(GENERATED_AUDIO_STORE);
      const request = callback(store);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error || new Error("Audio storage request failed."));
      transaction.onerror = () =>
        reject(transaction.error || new Error("Audio storage transaction failed."));
    });
  } finally {
    db.close();
  }
}

/**
 * Persist the last generated audio blob.
 *
 * Ports `saveLastGeneratedAudio` (app.html line ~1716). Swallows errors (the
 * original wraps the write in a try/catch that ignores failures). `mimeType`
 * falls back to `blob.type || 'audio/wav'`; `createdAt` is an ISO timestamp.
 */
export async function saveLastGeneratedAudio(
  blob: Blob,
  generatedText: string,
  inputChanged: boolean,
): Promise<void> {
  try {
    await withGeneratedAudioStore("readwrite", (store) =>
      store.put({
        id: LAST_GENERATED_AUDIO_KEY,
        text: generatedText,
        blob,
        mimeType: blob.type || "audio/wav",
        inputChanged: Boolean(inputChanged),
        createdAt: new Date().toISOString(),
      }),
    );
  } catch {
    // Ignored, matching app.html behavior.
  }
}

/**
 * Read the last generated audio record, or `null` if none is stored.
 *
 * Ports the `withGeneratedAudioStore('readonly', store => store.get(...))`
 * lookup in `restoreLastGeneratedAudio` (app.html line ~1804). Returns `null`
 * on any failure.
 */
export async function getLastGeneratedAudio(): Promise<GeneratedAudioRecord | null> {
  try {
    const record = await withGeneratedAudioStore<GeneratedAudioRecord | undefined>(
      "readonly",
      (store) =>
        store.get(LAST_GENERATED_AUDIO_KEY) as IDBRequest<GeneratedAudioRecord | undefined>,
    );
    return record ?? null;
  } catch {
    return null;
  }
}

/**
 * Delete the last generated audio record.
 *
 * Ports `deleteLastGeneratedAudio` (app.html line ~1729); errors are ignored.
 */
export async function deleteLastGeneratedAudio(): Promise<void> {
  try {
    await withGeneratedAudioStore("readwrite", (store) => store.delete(LAST_GENERATED_AUDIO_KEY));
  } catch {
    // Ignored, matching app.html behavior.
  }
}
