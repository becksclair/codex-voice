import type { ClipboardEvent, RefObject } from "react";

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
    <div className="text-shell">
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
      ></textarea>
      <button
        id="paste"
        type="button"
        className="icon-button"
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
        className="secondary icon-button"
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
