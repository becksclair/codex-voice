/**
 * Service-worker update reload coordinator.
 *
 * Ports the `serviceWorkerRefreshing`/`pendingWorkerReload`/
 * `reloadForWorkerUpdateWhenIdle` logic from app.html (lines ~793-794,
 * ~1406-1423, ~803-814). A new service worker taking control
 * (`controllerchange`) should reload the page, but only once the app is idle
 * (not generating and not streaming), so an in-flight generation or live
 * playback is never interrupted by the swap.
 */

let pending = false;
let refreshing = false;
let isBusy: () => boolean = () => false;

const UPDATE_INTERVAL_MS = 15 * 60 * 1000;
const UPDATE_MIN_GAP_MS = 60 * 1000;
const APP_MODE_WORKER_RELOAD_KEY = "codex-voice.web.app-mode-worker-cleanup";
const WORKER_UPDATE_NOTICE_KEY = "codex-voice.web.worker-update-notice";

type NoticeStorage = Pick<Storage, "getItem" | "removeItem" | "setItem">;

interface UpdateCheckOptions {
  intervalMs?: number;
  minGapMs?: number;
}

interface AppModeWorkerCleanupOptions {
  serviceWorker?: Pick<ServiceWorkerContainer, "controller" | "getRegistrations"> | null;
  storage?: Pick<Storage, "getItem" | "removeItem" | "setItem">;
  origin?: string;
  reload?: () => void;
}

/**
 * Remove `/web` workers left by older builds before app mode stopped
 * registering them. An unregister does not release the current controller
 * until the next navigation, so a controlled page reloads exactly once.
 */
export async function removeLegacyAppModeServiceWorkers(
  options: AppModeWorkerCleanupOptions = {},
): Promise<void> {
  const serviceWorker =
    options.serviceWorker ?? ("serviceWorker" in navigator ? navigator.serviceWorker : null);
  if (!serviceWorker) return;
  const storage = options.storage ?? sessionStorage;
  const origin = options.origin ?? location.origin;
  const reload = options.reload ?? (() => location.reload());
  try {
    const registrations = await serviceWorker.getRegistrations();
    const webScope = new URL("/web", origin).href;
    const matching = registrations.filter((registration) =>
      registration.scope.startsWith(webScope),
    );
    if (!matching.length) {
      storage.removeItem(APP_MODE_WORKER_RELOAD_KEY);
      return;
    }
    const results = await Promise.all(matching.map((registration) => registration.unregister()));
    if (
      results.some(Boolean) &&
      serviceWorker.controller &&
      storage.getItem(APP_MODE_WORKER_RELOAD_KEY) !== "1"
    ) {
      storage.setItem(APP_MODE_WORKER_RELOAD_KEY, "1");
      reload();
    }
  } catch {
    // Legacy cleanup is best-effort; app mode still never registers a worker.
  }
}

/**
 * Keep a long-running installed PWA checking for new service-worker builds.
 * Check immediately on every launch, then again when the app returns to the
 * foreground and on a bounded interval while it stays open. The explicit
 * no-store probe avoids browser registration throttling serving an older
 * installed PWA shell after a cold reopen.
 */
export function startServiceWorkerUpdateChecks(
  swUrl: string,
  registration: ServiceWorkerRegistration,
  options: UpdateCheckOptions = {},
): () => void {
  const intervalMs = options.intervalMs ?? UPDATE_INTERVAL_MS;
  const minGapMs = options.minGapMs ?? UPDATE_MIN_GAP_MS;
  let lastCheck = Number.NEGATIVE_INFINITY;
  let checking = false;

  const check = async (): Promise<void> => {
    const now = Date.now();
    if (checking || registration.installing || !navigator.onLine || now - lastCheck < minGapMs)
      return;
    checking = true;
    lastCheck = now;
    try {
      const response = await fetch(swUrl, {
        cache: "no-store",
        headers: { cache: "no-store", "cache-control": "no-cache" },
      });
      if (response.ok) await registration.update();
    } catch {
      // Updates are best-effort; the next foreground/interval check retries.
    } finally {
      checking = false;
    }
  };
  const checkWhenVisible = (): void => {
    if (document.visibilityState === "visible") void check();
  };
  const timer = window.setInterval(() => void check(), intervalMs);
  window.addEventListener("focus", checkWhenVisible);
  document.addEventListener("visibilitychange", checkWhenVisible);
  void check();

  return () => {
    window.clearInterval(timer);
    window.removeEventListener("focus", checkWhenVisible);
    document.removeEventListener("visibilitychange", checkWhenVisible);
  };
}

/**
 * Register the predicate that reports whether the app is busy (generating or
 * streaming). While it returns `true`, a queued reload is deferred.
 */
export function setBusyPredicate(fn: () => boolean): void {
  isBusy = fn;
}

/** Persist a one-navigation notice so the refreshed app can confirm its update. */
export function markWorkerUpdateNotice(storage?: NoticeStorage): void {
  try {
    (storage ?? sessionStorage).setItem(WORKER_UPDATE_NOTICE_KEY, "1");
  } catch {
    // Update activation must not fail when browser storage is unavailable.
  }
}

/** Whether the current navigation followed a worker-triggered update reload. */
export function hasWorkerUpdateNotice(storage?: NoticeStorage): boolean {
  try {
    return (storage ?? sessionStorage).getItem(WORKER_UPDATE_NOTICE_KEY) === "1";
  } catch {
    return false;
  }
}

/** Clear the one-navigation worker update notice after the refreshed app mounts. */
export function clearWorkerUpdateNotice(storage?: NoticeStorage): void {
  try {
    (storage ?? sessionStorage).removeItem(WORKER_UPDATE_NOTICE_KEY);
  } catch {
    // Best-effort only.
  }
}

/**
 * Reload for a pending worker update when the app is idle. No-op if nothing is
 * pending or the app is still busy. Ports `reloadForWorkerUpdateWhenIdle`.
 */
export function reloadForWorkerUpdateWhenIdle(): void {
  if (!pending || isBusy()) return;
  pending = false;
  refreshing = true;
  markWorkerUpdateNotice();
  window.location.reload();
}

/**
 * Queue a worker-update reload and attempt it immediately. Called from the
 * `controllerchange` handler. Ports the `controllerchange` listener body.
 */
export function requestWorkerReload(): void {
  if (refreshing) return;
  pending = true;
  reloadForWorkerUpdateWhenIdle();
}
