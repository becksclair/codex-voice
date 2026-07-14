import { expect, test } from "vitest";
import { backendNavigationDenylist, proxyTargets } from "./vite.config.ts";

test("the dev server proxies every backend-owned web route", () => {
  expect(proxyTargets).toEqual([
    "/web/config",
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
