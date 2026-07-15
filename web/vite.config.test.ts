import { expect, test } from "vitest";
import { backendNavigationDenylist, proxyTargets, pwaWorkboxOptions } from "./vite.config.ts";

test("the dev server proxies every backend-owned web route", () => {
  expect(proxyTargets).toEqual([
    "/web/config",
    "/web/codex-auth",
    "/web/speech",
    "/web/speech-jobs",
    "/web/desktop-intents",
  ]);
});

test("the service worker never serves the app shell for backend routes", () => {
  for (const path of proxyTargets) {
    expect(backendNavigationDenylist.some((pattern) => pattern.test(path))).toBe(true);
  }
  expect(backendNavigationDenylist.some((pattern) => pattern.test("/web/"))).toBe(false);
});

test("manual auto-update registration activates and claims new workers immediately", () => {
  expect(pwaWorkboxOptions.skipWaiting).toBe(true);
  expect(pwaWorkboxOptions.clientsClaim).toBe(true);
});
