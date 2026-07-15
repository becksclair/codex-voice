import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { removeLegacyAppModeServiceWorkers, startServiceWorkerUpdateChecks } from "./pwa.ts";

const cleanup: Array<() => void> = [];

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-07-15T00:00:00Z"));
});

afterEach(() => {
  cleanup.splice(0).forEach((dispose) => dispose());
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

function registration(update = vi.fn(async () => {})): ServiceWorkerRegistration {
  return { installing: null, update } as unknown as ServiceWorkerRegistration;
}

test("checks for an updated worker periodically with cache bypassed", async () => {
  const update = vi.fn(async () => {});
  const fetchMock = vi.fn(async () => new Response("worker", { status: 200 }));
  vi.stubGlobal("fetch", fetchMock);
  cleanup.push(
    startServiceWorkerUpdateChecks("/web/sw.js", registration(update), {
      intervalMs: 1_000,
      minGapMs: 1_000,
    }),
  );

  await vi.advanceTimersByTimeAsync(1_000);

  expect(fetchMock).toHaveBeenCalledWith("/web/sw.js", {
    cache: "no-store",
    headers: { cache: "no-store", "cache-control": "no-cache" },
  });
  expect(update).toHaveBeenCalledOnce();
});

test("checks when the PWA returns to the foreground", async () => {
  const update = vi.fn(async () => {});
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response("worker", { status: 200 })),
  );
  cleanup.push(
    startServiceWorkerUpdateChecks("/web/sw.js", registration(update), {
      intervalMs: 60_000,
      minGapMs: 1_000,
    }),
  );

  await vi.advanceTimersByTimeAsync(1_000);
  window.dispatchEvent(new Event("focus"));
  await vi.waitFor(() => expect(update).toHaveBeenCalledOnce());
});

test("skips checks while offline or while another worker is installing", async () => {
  const update = vi.fn(async () => {});
  const fetchMock = vi.fn(async () => new Response("worker", { status: 200 }));
  vi.stubGlobal("fetch", fetchMock);
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
  const activeRegistration = registration(update);
  cleanup.push(
    startServiceWorkerUpdateChecks("/web/sw.js", activeRegistration, {
      intervalMs: 1_000,
      minGapMs: 1_000,
    }),
  );

  await vi.advanceTimersByTimeAsync(1_000);
  expect(fetchMock).not.toHaveBeenCalled();

  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  Object.defineProperty(activeRegistration, "installing", { value: {}, configurable: true });
  await vi.advanceTimersByTimeAsync(1_000);
  expect(fetchMock).not.toHaveBeenCalled();
});

test("removes a legacy web worker and reloads a controlled app-mode page once", async () => {
  const unregister = vi.fn(async () => true);
  const reload = vi.fn();
  const storage = new Map<string, string>();
  const storageAdapter = {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => void storage.set(key, value),
    removeItem: (key: string) => void storage.delete(key),
  };
  await removeLegacyAppModeServiceWorkers({
    serviceWorker: {
      controller: {} as ServiceWorker,
      getRegistrations: async () =>
        [
          { scope: "https://voice.example/web/", unregister },
          { scope: "https://voice.example/other/", unregister: vi.fn(async () => true) },
        ] as unknown as ServiceWorkerRegistration[],
    },
    storage: storageAdapter,
    origin: "https://voice.example",
    reload,
  });

  expect(unregister).toHaveBeenCalledOnce();
  expect(reload).toHaveBeenCalledOnce();
});
