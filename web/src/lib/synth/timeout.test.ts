import { afterEach, describe, expect, it, vi } from "vitest";
import { providerTimeoutSignal, ttsTimeoutForInput } from "./timeout.ts";

afterEach(() => {
  vi.useRealTimers();
});

describe("ttsTimeoutForInput", () => {
  it("matches the Rust short, scaled, and capped timeout policy", () => {
    expect(ttsTimeoutForInput(30_000, "a".repeat(1_200))).toBe(30_000);
    expect(ttsTimeoutForInput(30_000, "a".repeat(4_000))).toBe(160_000);
    expect(ttsTimeoutForInput(30_000, "a".repeat(20_000))).toBe(300_000);
  });
});

describe("providerTimeoutSignal", () => {
  it("aborts after the resolved provider timeout", () => {
    vi.useFakeTimers();
    const timed = providerTimeoutSignal(30_000, "hello", null);
    vi.advanceTimersByTime(29_999);
    expect(timed.signal.aborted).toBe(false);
    vi.advanceTimersByTime(1);
    expect(timed.signal.aborted).toBe(true);
    expect(timed.signal.reason).toMatchObject({ name: "TimeoutError" });
    timed.dispose();
  });

  it("propagates external cancellation and removes its listener on dispose", () => {
    vi.useFakeTimers();
    const external = new AbortController();
    const timed = providerTimeoutSignal(30_000, "hello", external.signal);
    external.abort("cancelled");
    expect(timed.signal.aborted).toBe(true);
    expect(timed.signal.reason).toBe("cancelled");
    timed.dispose();
  });
});
