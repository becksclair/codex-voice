import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { registerSW } from "virtual:pwa-register";
import { App } from "./App.tsx";
import { isAppMode } from "./lib/appMode.ts";
import {
  removeLegacyAppModeServiceWorkers,
  requestWorkerReload,
  startServiceWorkerUpdateChecks,
} from "./pwa.ts";
import "./index.css";

// Desktop app webviews (Tauri, loaded with `?app=1`) never register the
// service worker: the PWA install/update/offline machinery is browser-only
// behavior that has no meaning inside a native window, and there is no
// `window.__TAURI__` or IPC bridge to coordinate a SW update reload against.
if (isAppMode(location.search)) {
  void removeLegacyAppModeServiceWorkers();
} else {
  registerSW({
    immediate: true,
    onNeedReload: requestWorkerReload,
    onRegisteredSW: (swUrl, registration) => {
      if (registration) startServiceWorkerUpdateChecks(swUrl, registration);
    },
  });

  // A new service worker taking control should reload the page to pick up the new
  // build, but only once the app is idle (not generating or streaming). Ports the
  // `controllerchange` reload behavior from app.html (lines ~809-814); the
  // idle-deferral logic lives in ./pwa.ts.
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.addEventListener("controllerchange", () => {
      requestWorkerReload();
    });
  }
}

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("Root element #root not found");
}

createRoot(rootElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
