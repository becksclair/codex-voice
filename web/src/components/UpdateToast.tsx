import { useEffect, useState } from "react";

export const UPDATE_TOAST_DURATION_MS = 5_000;

/** Brief confirmation shown after a service-worker update reload. */
export function UpdateToast() {
  const [visible, setVisible] = useState(true);

  useEffect(() => {
    const timer = window.setTimeout(() => setVisible(false), UPDATE_TOAST_DURATION_MS);
    return () => window.clearTimeout(timer);
  }, []);

  if (!visible) return null;

  return (
    <div
      id="update-toast"
      className="fixed top-[max(12px,env(safe-area-inset-top))] left-1/2 z-50 flex -translate-x-1/2 items-center gap-2 rounded-full border border-[var(--glass-button-border)] bg-[image:var(--glass-button-bg)] py-2 pr-2 pl-3.5 text-[0.82rem] font-semibold whitespace-nowrap text-[var(--text)] shadow-[var(--glass-button-shadow)] [backdrop-filter:var(--glass-button-filter)] [-webkit-backdrop-filter:var(--glass-button-filter)]"
      role="status"
      aria-live="polite"
    >
      <span>Updated to latest version</span>
      <button
        type="button"
        className="flex h-7 w-7 cursor-pointer items-center justify-center rounded-full border-0 bg-transparent p-0 text-lg leading-none text-[var(--text)]"
        aria-label="Dismiss update notice"
        onClick={() => setVisible(false)}
      >
        ×
      </button>
    </div>
  );
}
