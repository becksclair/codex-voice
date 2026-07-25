import { expect, test } from "vitest";
import { proxyTargets } from "./vite.config.ts";

test("the dev server proxies every backend-owned web route", () => {
  expect(proxyTargets).toEqual([
    "/web/config",
    "/web/google-stream",
    "/web/speech",
    "/web/speech-jobs",
    "/web/desktop-intents",
  ]);
});
