import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchCodexPrepAttempt, parseCodexSse, refreshCodexAuth } from "./codex.ts";
import type { EffectiveSpeechPrep } from "./types.ts";

function jwt(exp: number): string {
  return `header.${btoa(JSON.stringify({ exp }))}.signature`;
}

function prep(accessToken = jwt(Math.floor(Date.now() / 1000) + 3600)): EffectiveSpeechPrep {
  return {
    provider: "codex",
    mode: "performance-tags",
    strategies: { google: "inline-tags", elevenlabs: "inline-tags", default: "inline-tags" },
    tagPalette: [],
    capPerformanceTags: false,
    browserSupported: true,
    baseUrl: "/_codex",
    model: "gpt-test",
    fallbackModels: [],
    threshold: 0,
    maxInputLength: 1000,
    maxLength: 1000,
    attemptTimeoutMs: 1000,
    timeoutMs: 5000,
    codexAuth: {
      accessToken,
      refreshToken: "refresh-token",
      accountId: "account-id",
      tokenUrl: "https://auth.example.test/oauth/token",
      clientId: "client-id",
    },
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("Codex browser transport", () => {
  it("posts the expected Responses request to the same-origin relay", async () => {
    const fetchMock = vi.fn(async () => new Response("data: [DONE]\n\n"));
    vi.stubGlobal("fetch", fetchMock);
    const config = prep();

    await fetchCodexPrepAttempt(config, "codex/gpt-test", "Transform this", null);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/_codex/responses");
    expect(init.method).toBe("POST");
    expect(init.headers).toMatchObject({
      Authorization: `Bearer ${config.codexAuth?.accessToken}`,
      "chatgpt-account-id": "account-id",
      originator: "codex-voice-web",
      Accept: "text/event-stream",
    });
    expect(JSON.parse(String(init.body))).toMatchObject({
      model: "gpt-test",
      store: false,
      stream: true,
    });
  });

  it("refreshes an expired bundle and reports the rotated credentials", async () => {
    const config = prep(jwt(1));
    const onRefreshed = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              access_token: jwt(Math.floor(Date.now() / 1000) + 7200),
              refresh_token: "rotated-refresh",
              account_id: "account-id",
            }),
            { status: 200 },
          ),
      ),
    );

    const auth = await refreshCodexAuth(config, onRefreshed);

    expect(auth.refreshToken).toBe("rotated-refresh");
    expect(auth.serverSyncPending).toBe(true);
    expect(onRefreshed).toHaveBeenCalledWith(config);
  });

  it("parses streamed Codex response text", () => {
    expect(
      parseCodexSse(
        'data: {"type":"response.output_text.delta","delta":"Hello "}\n' +
          'data: {"type":"response.output_text.delta","delta":"world"}\n' +
          "data: [DONE]\n",
      ),
    ).toBe("Hello world");
  });
});
