import { useEffect, useState } from "react";
import {
  fetchConfig,
  loadCachedConfig,
  reconcileBrowserConfig,
  saveCachedConfig,
  syncCodexAuthToServer,
  type BrowserTtsConfig,
} from "../lib/index.ts";

/**
 * The live browser-TTS config: cached value first, then a background refresh.
 *
 * Ports the `directConfig = loadCachedConfig()` seed and the `refreshConfig`
 * fetch from the legacy mount effect. The fetched config is cached and becomes
 * the new state; downstream hooks react to the change (settings repopulate, the
 * generation controller updates).
 */
export function useServerConfig(): BrowserTtsConfig | null {
  const [config, setConfig] = useState<BrowserTtsConfig | null>(loadCachedConfig);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const fresh = await fetchConfig();
      if (cancelled || !fresh) return;
      setConfig((current) => {
        const reconciled = reconcileBrowserConfig(fresh, current);
        saveCachedConfig(reconciled);
        void syncCodexAuthToServer(reconciled);
        return reconciled;
      });
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return config;
}
