import { describe, expect, it, vi } from "vitest";
import { consumeDesktopIntent, isAppMode, settingsView, speakIntentId } from "./appMode.ts";

describe("isAppMode", () => {
  it("is false with no query string", () => {
    expect(isAppMode("")).toBe(false);
  });

  it("is false when app is not exactly '1'", () => {
    expect(isAppMode("?app=true")).toBe(false);
    expect(isAppMode("?app=0")).toBe(false);
  });

  it("is true when app=1 is present", () => {
    expect(isAppMode("?app=1")).toBe(true);
    expect(isAppMode("?view=settings&app=1")).toBe(true);
  });
});

describe("settingsView", () => {
  it("is false with no query string", () => {
    expect(settingsView("")).toBe(false);
  });

  it("is false for other view values", () => {
    expect(settingsView("?view=other")).toBe(false);
  });

  it("is true when view=settings is present", () => {
    expect(settingsView("?view=settings")).toBe(true);
    expect(settingsView("?app=1&view=settings")).toBe(true);
  });
});

describe("speakIntentId", () => {
  it("returns null for an empty hash", () => {
    expect(speakIntentId("")).toBeNull();
    expect(speakIntentId("#")).toBeNull();
  });

  it("returns null when intent is absent or malformed", () => {
    expect(speakIntentId("#text=hello")).toBeNull();
    expect(speakIntentId("#intent=short")).toBeNull();
  });

  it("normalizes a valid id", () => {
    expect(speakIntentId("#intent=ABCDEF0123456789ABCDEF0123456789")).toBe(
      "abcdef0123456789abcdef0123456789",
    );
  });
});

describe("consumeDesktopIntent", () => {
  it("returns consumed text", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ text: "héllo 日本語" }))),
    );
    await expect(consumeDesktopIntent("a".repeat(32))).resolves.toBe("héllo 日本語");
  });

  it("surfaces the server error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ error: { message: "expired" } }), { status: 404 }),
      ),
    );
    await expect(consumeDesktopIntent("a".repeat(32))).rejects.toThrow("expired");
  });
});
