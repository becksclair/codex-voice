import { useEffect, useState } from "react";
import { fetchConfig, type BrowserTtsConfig } from "../lib/index.ts";

/**
 * Fetch the live browser-TTS config once at mount. It is deliberately not
 * cached; downstream hooks reconcile settings when the request completes.
 */
export interface ServerConfigState {
  config: BrowserTtsConfig | null;
  error: string;
}

export function useServerConfig(): ServerConfigState {
  const [config, setConfig] = useState<BrowserTtsConfig | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const fresh = await fetchConfig();
        if (!cancelled) setConfig(fresh);
      } catch (cause) {
        if (!cancelled) setError((cause as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return { config, error };
}
