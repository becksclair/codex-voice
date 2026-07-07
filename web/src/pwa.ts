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

/**
 * Register the predicate that reports whether the app is busy (generating or
 * streaming). While it returns `true`, a queued reload is deferred.
 */
export function setBusyPredicate(fn: () => boolean): void {
  isBusy = fn;
}

/**
 * Reload for a pending worker update when the app is idle. No-op if nothing is
 * pending or the app is still busy. Ports `reloadForWorkerUpdateWhenIdle`.
 */
export function reloadForWorkerUpdateWhenIdle(): void {
  if (!pending || isBusy()) return;
  pending = false;
  refreshing = true;
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
