import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ServerJobError,
  SERVER_JOB_POLL_MS,
  cancelWebSpeechJob,
  type WebSpeechJobStatus,
  createWebSpeechJob,
  fetchWebSpeechJob,
  waitForWebSpeechJob,
} from "./serverJobs.ts";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("createWebSpeechJob", () => {
  it("posts input and returns the job id", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ id: "job-1", status: "pending" }));
    vi.stubGlobal("fetch", fetchMock);
    const id = await createWebSpeechJob("hello");
    expect(id).toBe("job-1");
    expect(fetchMock).toHaveBeenCalledWith(
      "/web/speech-jobs",
      expect.objectContaining({ method: "POST", body: JSON.stringify({ input: "hello" }) }),
    );
  });

  it("throws the server error message on failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ error: { message: "too long" } }, 400)),
    );
    await expect(createWebSpeechJob("x")).rejects.toThrow("too long");
  });

  it("throws a status message when the error body is unparseable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("boom", { status: 503 })),
    );
    await expect(createWebSpeechJob("x")).rejects.toThrow("TTS job failed (503)");
  });

  it("throws when no id is returned", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ status: "pending" })),
    );
    await expect(createWebSpeechJob("x")).rejects.toThrow("TTS job did not return an id.");
  });
});

describe("cancelWebSpeechJob", () => {
  it("deletes the encoded job id with keepalive", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    await cancelWebSpeechJob("job/1");
    expect(fetchMock).toHaveBeenCalledWith(
      "/web/speech-jobs/job%2F1",
      expect.objectContaining({ method: "DELETE", keepalive: true }),
    );
  });
});

describe("fetchWebSpeechJob", () => {
  it("fetches with no-store cache", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ id: "j", status: "pending" }));
    vi.stubGlobal("fetch", fetchMock);
    const job = await fetchWebSpeechJob("job-abc");
    expect(job.status).toBe("pending");
    expect(fetchMock).toHaveBeenCalledWith(
      "/web/speech-jobs/job-abc",
      expect.objectContaining({ cache: "no-store" }),
    );
  });

  it("throws a status message on a non-OK response", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("x", { status: 404 })),
    );
    await expect(fetchWebSpeechJob("j")).rejects.toThrow("TTS job status failed (404)");
  });
});

describe("waitForWebSpeechJob", () => {
  const result = {
    input: "hi",
    input_changed: false,
    audio_base64: "AQID",
    mime_type: "audio/wav",
    format: "wav",
  };

  it("returns the result when the job completes immediately", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ id: "j", status: "complete", result })),
    );
    expect(await waitForWebSpeechJob("j")).toEqual(result);
  });

  it("polls until the job completes", async () => {
    vi.useFakeTimers();
    const responses: WebSpeechJobStatus[] = [
      { id: "j", status: "pending" },
      { id: "j", status: "pending" },
      { id: "j", status: "complete", result },
    ];
    let call = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse(responses[Math.min(call++, responses.length - 1)])),
    );
    const onProgress = vi.fn();
    const promise = waitForWebSpeechJob("j", { onProgress });
    // Drive the two poll intervals.
    await vi.advanceTimersByTimeAsync(SERVER_JOB_POLL_MS);
    await vi.advanceTimersByTimeAsync(SERVER_JOB_POLL_MS);
    expect(await promise).toEqual(result);
    expect(onProgress).toHaveBeenCalledWith(0.82, "Loading");
    expect(onProgress).toHaveBeenCalledWith(0.64, "Synthesizing");
  });

  it("throws the job error on failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ id: "j", status: "failed", error: { message: "nope" } })),
    );
    await expect(waitForWebSpeechJob("j")).rejects.toThrow("nope");
  });

  it("throws a 408 ServerJobError once the max poll window elapses", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ id: "j", status: "pending" })),
    );
    const promise = waitForWebSpeechJob("j").catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(11 * 60 * 1000);
    const error = await promise;
    expect(error).toBeInstanceOf(ServerJobError);
    expect((error as ServerJobError).status).toBe(408);
  });

  it("stops polling when throwIfCancelled throws", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ id: "j", status: "pending" })),
    );
    await expect(
      waitForWebSpeechJob("j", {
        throwIfCancelled: () => {
          throw new Error("cancelled");
        },
      }),
    ).rejects.toThrow("cancelled");
  });
});
