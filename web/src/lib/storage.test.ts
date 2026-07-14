import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  GENERATION_STATE_STORAGE_KEY,
  MAX_PENDING_GENERATION_AGE_MS,
  clearPendingGeneration,
  deleteLastGeneratedAudio,
  getLastGeneratedAudio,
  loadPendingGeneration,
  loadText,
  savePendingGeneration,
  saveLastGeneratedAudio,
  saveText,
} from "./storage.ts";

beforeEach(() => {
  localStorage.clear();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("text persistence", () => {
  it("defaults to an empty string", () => {
    expect(loadText()).toBe("");
  });

  it("round-trips saved text", () => {
    saveText("hello world");
    expect(loadText()).toBe("hello world");
  });
});

describe("pending generation", () => {
  it("returns null when nothing is stored", () => {
    expect(loadPendingGeneration()).toBeNull();
  });

  it("round-trips a server-job pending generation", () => {
    savePendingGeneration("some input", "job-123");
    const pending = loadPendingGeneration();
    expect(pending?.input).toBe("some input");
    expect(pending?.jobId).toBe("job-123");
    expect(typeof pending?.startedAt).toBe("number");
  });

  it("discards and clears a pending generation without a jobId", () => {
    savePendingGeneration("no job");
    expect(loadPendingGeneration()).toBeNull();
    expect(localStorage.getItem(GENERATION_STATE_STORAGE_KEY)).toBeNull();
  });

  it("discards a pending generation older than the max age", () => {
    savePendingGeneration("stale", "job-9");
    const now = Date.now();
    vi.useFakeTimers();
    vi.setSystemTime(now + MAX_PENDING_GENERATION_AGE_MS + 1000);
    expect(loadPendingGeneration()).toBeNull();
    expect(localStorage.getItem(GENERATION_STATE_STORAGE_KEY)).toBeNull();
  });

  it("tolerates corrupt JSON and clears it", () => {
    localStorage.setItem(GENERATION_STATE_STORAGE_KEY, "{broken");
    expect(loadPendingGeneration()).toBeNull();
    expect(localStorage.getItem(GENERATION_STATE_STORAGE_KEY)).toBeNull();
  });

  it("clearPendingGeneration removes the record", () => {
    savePendingGeneration("x", "job-1");
    clearPendingGeneration();
    expect(localStorage.getItem(GENERATION_STATE_STORAGE_KEY)).toBeNull();
  });

  it("only clears pending generation owned by the completed run", () => {
    savePendingGeneration("replacement", "job-2", "new-owner");
    clearPendingGeneration("old-owner");
    expect(loadPendingGeneration()?.jobId).toBe("job-2");

    clearPendingGeneration("new-owner");
    expect(loadPendingGeneration()).toBeNull();
  });
});

describe("generated audio IndexedDB store", () => {
  it("saves, reads, and deletes the last generated audio", async () => {
    const blob = new Blob([new Uint8Array([1, 2, 3])], { type: "audio/wav" });
    await saveLastGeneratedAudio(blob, "the text", true);

    const record = await getLastGeneratedAudio();
    expect(record?.id).toBe("last");
    expect(record?.text).toBe("the text");
    expect(record?.mimeType).toBe("audio/wav");
    expect(record?.inputChanged).toBe(true);
    expect(typeof record?.createdAt).toBe("string");
    // The blob is round-tripped through IndexedDB's structured clone. The
    // fake-indexeddb polyfill degrades Blob to a plain shape, so we assert the
    // record round-trips rather than re-reading blob bytes here.
    expect(record?.blob).toBeDefined();

    await deleteLastGeneratedAudio();
    expect(await getLastGeneratedAudio()).toBeNull();
  });

  it("returns null when nothing is stored", async () => {
    // Clean any prior record from a shared IndexedDB.
    await deleteLastGeneratedAudio();
    expect(await getLastGeneratedAudio()).toBeNull();
  });

  it("only conditionally deletes audio owned by the cancelled run", async () => {
    const blob = new Blob([new Uint8Array([1])], { type: "audio/wav" });
    await saveLastGeneratedAudio(blob, "new run", false, "new-owner");

    await deleteLastGeneratedAudio("old-owner");
    expect((await getLastGeneratedAudio())?.text).toBe("new run");

    await deleteLastGeneratedAudio("new-owner");
    expect(await getLastGeneratedAudio()).toBeNull();
  });
});
