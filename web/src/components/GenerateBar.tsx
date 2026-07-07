import type { CSSProperties } from "react";

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
  return (
    <div className="buttons">
      <button
        id="generate"
        type="button"
        className={props.generating ? "generating" : undefined}
        style={{ "--generate-progress": props.progress } as CSSProperties}
        disabled={props.generateDisabled}
        onClick={props.onGenerate}
      >
        <span className="generate-main">
          <span className="spinner" aria-hidden="true"></span>
          <span id="generate-label">{props.label}</span>
        </span>
        <span className="generate-progress" aria-hidden="true">
          <span></span>
        </span>
      </button>
      <button
        id="play"
        type="button"
        className="secondary icon-button"
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
        className="secondary icon-button"
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
        className="secondary icon-button"
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
