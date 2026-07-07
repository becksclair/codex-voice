import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { registerSW } from "virtual:pwa-register";
import { App } from "./App.tsx";
import { requestWorkerReload } from "./pwa.ts";
import "./index.css";

registerSW({ immediate: true });

// A new service worker taking control should reload the page to pick up the new
// build, but only once the app is idle (not generating or streaming). Ports the
// `controllerchange` reload behavior from app.html (lines ~809-814); the
// idle-deferral logic lives in ./pwa.ts.
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.addEventListener("controllerchange", () => {
    requestWorkerReload();
  });
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
