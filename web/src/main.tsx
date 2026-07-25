import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App.tsx";
import { clearLegacyPersistentState } from "./lib/storage.ts";
import "./index.css";

const rootElement = document.getElementById("root");
if (!rootElement) throw new Error("Root element #root not found");
clearLegacyPersistentState();

createRoot(rootElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
