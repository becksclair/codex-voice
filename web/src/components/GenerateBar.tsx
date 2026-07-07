import type { CSSProperties } from "react";

/**
 * Shared utilities for the frosted-glass secondary icon buttons (play, download,
 * settings). `icon-button` is retained as a hook for the sheen pseudo-elements
 * and the raised-svg rule in index.css.
 */
const GLASS_ICON_BUTTON =
  "icon-button relative inline-flex h-11 min-h-11 w-11 min-w-11 cursor-pointer touch-manipulation items-center justify-center overflow-hidden rounded-[13px] border border-[var(--glass-button-border)] bg-[image:var(--glass-button-bg)] p-0 text-[var(--glass-button-color)] shadow-[var(--glass-button-shadow)] [backdrop-filter:var(--glass-button-filter)] [-webkit-backdrop-filter:var(--glass-button-filter)] hover:border-[var(--glass-button-hover-border)] hover:bg-[image:var(--glass-button-hover-bg)] disabled:cursor-not-allowed disabled:opacity-[0.72]";

interface GenerateBarProps {
  generating: boolean;
  progress: number;
  label: string;
  generateDisabled: boolean;
  onGenerate: () => void;
  paused: boolean;
  playDisabled: boolean;
  onTogglePlay: () => void;
  downloadDisabled: boolean;
  onDownload: () => void;
  settingsOpen: boolean;
  onToggleSettings: () => void;
}

/**
 * The primary control row: generate (`#generate`), play/pause (`#play`,
 * `#play-icon`), download (`#download`), and the settings toggle (`#settings-toggle`).
 */
export function GenerateBar(props: GenerateBarProps) {
  const generateClass =
    "relative flex h-11 min-h-11 items-center justify-center overflow-hidden rounded-[18px] border-0 bg-[var(--accent)] px-2.5 text-[0.98rem] font-bold text-[var(--button-text)] cursor-pointer touch-manipulation shadow-[var(--button-shadow)] disabled:cursor-not-allowed disabled:opacity-[0.55]";

  return (
    <div className="buttons grid grid-cols-[minmax(0,1fr)_20px_repeat(3,44px)] items-center gap-x-3 gap-y-2">
      <button
        id="generate"
        type="button"
        className={props.generating ? `${generateClass} generating` : generateClass}
        style={{ "--generate-progress": props.progress } as CSSProperties}
        disabled={props.generateDisabled}
        onClick={props.onGenerate}
      >
        <span className="relative z-[1] inline-flex min-w-0 items-center justify-center gap-2">
          <span className="spinner" aria-hidden="true"></span>
          <span id="generate-label">{props.label}</span>
        </span>
        <span
          className="generate-progress absolute right-0 bottom-0 left-0 h-[3px] overflow-hidden bg-[var(--progress-track)]"
          aria-hidden="true"
        >
          <span></span>
        </span>
      </button>
      <button
        id="play"
        type="button"
        className={`${GLASS_ICON_BUTTON} col-start-3`}
        disabled={props.playDisabled}
        aria-label={props.paused ? "Play" : "Pause"}
        onClick={props.onTogglePlay}
      >
        <svg id="play-icon" viewBox="0 0 24 24" aria-hidden="true">
          {props.paused ? (
            <path d="M8 5v14l11-7Z" />
          ) : (
            <>
              <path d="M8 5v14" />
              <path d="M16 5v14" />
            </>
          )}
        </svg>
      </button>
      <button
        id="download"
        type="button"
        className={GLASS_ICON_BUTTON}
        disabled={props.downloadDisabled}
        aria-label="Download audio"
        onClick={props.onDownload}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 3v12" />
          <path d="m7 10 5 5 5-5" />
          <path d="M5 21h14" />
        </svg>
      </button>
      <button
        id="settings-toggle"
        type="button"
        className={GLASS_ICON_BUTTON}
        aria-label="Toggle settings"
        aria-expanded={props.settingsOpen}
        onClick={props.onToggleSettings}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.04 1.56V21a2 2 0 1 1-4 0v-.08a1.7 1.7 0 0 0-1.04-1.56 1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.56-1.04H3a2 2 0 1 1 0-4h.08A1.7 1.7 0 0 0 4.64 8.9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-1.56V3a2 2 0 1 1 4 0v.08a1.7 1.7 0 0 0 1.04 1.56 1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 9c.1.38.4.7.76.86.25.1.52.15.8.14H21a2 2 0 1 1 0 4h-.08A1.7 1.7 0 0 0 19.4 15Z" />
        </svg>
      </button>
    </div>
  );
}
