import type { ClipboardEvent, RefObject } from "react";

/**
 * Shared utilities for the translucent "overlay" icon buttons that float over
 * the textarea. `icon-button` is retained only as a hook for the `svg` styling.
 */
const OVERLAY_ICON_BUTTON =
  "icon-button inline-flex h-11 min-h-11 w-11 min-w-11 cursor-pointer touch-manipulation items-center justify-center rounded-full border border-[var(--overlay-border)] bg-[image:var(--overlay-bg)] p-0 text-[var(--overlay-color)] shadow-[var(--overlay-shadow)] [backdrop-filter:blur(6px)_saturate(1.25)] [-webkit-backdrop-filter:blur(6px)_saturate(1.25)] hover:border-[var(--overlay-hover-border)] hover:bg-[image:var(--overlay-hover-bg)] active:bg-[image:var(--overlay-active-bg)] active:shadow-[var(--overlay-active-shadow)]";

interface TextEditorProps {
  textRef: RefObject<HTMLTextAreaElement | null>;
  value: string;
  onChange: (value: string) => void;
  onPaste: (event: ClipboardEvent<HTMLTextAreaElement>) => void;
  onPasteClick: () => void;
  onClearClick: () => void;
  /** Whether the clear button is shown (text is non-empty). */
  clearVisible: boolean;
}

/** The prompt textarea (`#text`) with its paste (`#paste`) and clear (`#clear`) buttons. */
export function TextEditor(props: TextEditorProps) {
  return (
    <div className="text-shell relative flex flex-auto min-h-[260px] overflow-hidden rounded-[22px] border border-[var(--line)] bg-[var(--panel)] p-0 [--text-button-clearance:126px] [--text-edge-pad:8px] focus-within:border-[var(--accent)] focus-within:shadow-[0_0_0_3px_var(--focus-ring)]">
      <textarea
        id="text"
        ref={props.textRef}
        value={props.value}
        onChange={(event) => props.onChange(event.target.value)}
        onPaste={props.onPaste}
        autoComplete="off"
        autoCapitalize="sentences"
        spellCheck={true}
        placeholder="Type something to hear it spoken..."
        className="h-full min-h-0 w-full flex-auto resize-none rounded-none border-0 bg-transparent px-4 pt-[var(--text-edge-pad)] pb-[calc(var(--text-button-clearance)_+_var(--text-edge-pad))] text-[0.94rem] leading-[1.45] text-[var(--text)] outline-none [scroll-padding:var(--text-edge-pad)_16px_calc(var(--text-button-clearance)_+_var(--text-edge-pad))]"
      ></textarea>
      <button
        id="paste"
        type="button"
        className={`${OVERLAY_ICON_BUTTON} absolute right-2.5 bottom-2.5`}
        aria-label="Paste clipboard contents"
        onClick={props.onPasteClick}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M8 4h8" />
          <path d="M9 2h6a1 1 0 0 1 1 1v2H8V3a1 1 0 0 1 1-1Z" />
          <path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" />
        </svg>
      </button>
      <button
        id="clear"
        type="button"
        className={`${OVERLAY_ICON_BUTTON} absolute right-2.5 bottom-[70px]`}
        aria-label="Clear text"
        hidden={!props.clearVisible}
        onClick={props.onClearClick}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M3 6h18" />
          <path d="M8 6V4h8v2" />
          <path d="M19 6l-1 14H6L5 6" />
          <path d="M10 11v5" />
          <path d="M14 11v5" />
        </svg>
      </button>
    </div>
  );
}
