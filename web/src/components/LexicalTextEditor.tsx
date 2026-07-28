import { useEffect, useRef, type ClipboardEvent, type MutableRefObject } from "react";
import { LexicalComposer } from "@lexical/react/LexicalComposer";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import { ContentEditable } from "@lexical/react/LexicalContentEditable";
import { LexicalErrorBoundary } from "@lexical/react/LexicalErrorBoundary";
import { HistoryPlugin } from "@lexical/react/LexicalHistoryPlugin";
import { ListPlugin } from "@lexical/react/LexicalListPlugin";
import { MarkdownShortcutPlugin } from "@lexical/react/LexicalMarkdownShortcutPlugin";
import { OnChangePlugin } from "@lexical/react/LexicalOnChangePlugin";
import { RichTextPlugin } from "@lexical/react/LexicalRichTextPlugin";
import {
  $convertToMarkdownString,
  $convertFromMarkdownString,
  TRANSFORMERS,
} from "@lexical/markdown";
import { $isCodeNode, CodeHighlightNode, CodeNode } from "@lexical/code";
import { LinkNode } from "@lexical/link";
import { ListItemNode, ListNode } from "@lexical/list";
import { HeadingNode, QuoteNode } from "@lexical/rich-text";
import { TextNode, type EditorState, type LexicalEditor } from "lexical";

/** The source mirror preserves the existing generation-facing `.value` contract. */
export type TextMirrorElement = HTMLTextAreaElement;

/**
 * Shared utilities for the translucent "overlay" icon buttons that float over
 * the editor. `icon-button` is retained only as a hook for the `svg` styling.
 */
const OVERLAY_ICON_BUTTON =
  "icon-button inline-flex h-11 min-h-11 w-11 min-w-11 cursor-pointer touch-manipulation items-center justify-center rounded-full border border-[var(--overlay-border)] bg-[image:var(--overlay-bg)] p-0 text-[var(--overlay-color)] shadow-[var(--overlay-shadow)] [backdrop-filter:blur(10px)_saturate(1.2)] [-webkit-backdrop-filter:blur(10px)_saturate(1.2)] hover:border-[var(--overlay-hover-border)] hover:bg-[image:var(--overlay-hover-bg)] active:bg-[image:var(--overlay-active-bg)] active:shadow-[var(--overlay-active-shadow)]";

interface TextEditorProps {
  textRef: MutableRefObject<TextMirrorElement | null>;
  value: string;
  onChange: (value: string) => void;
  onPaste: (event: ClipboardEvent<HTMLElement>) => void;
  onPasteClick: () => void;
  onClearClick: () => void;
  /** Whether the clear button is shown (text is non-empty). */
  clearVisible: boolean;
}

const theme = {
  paragraph: "composer-paragraph",
  quote: "composer-quote",
  heading: {
    h1: "composer-heading composer-heading-h1",
    h2: "composer-heading composer-heading-h2",
    h3: "composer-heading composer-heading-h3",
    h4: "composer-heading composer-heading-h4",
    h5: "composer-heading composer-heading-h5",
    h6: "composer-heading composer-heading-h6",
  },
  list: {
    ul: "composer-list composer-list-ul",
    ol: "composer-list composer-list-ol",
    listitem: "composer-list-item",
    nested: { listitem: "composer-list-item" },
  },
  code: "composer-code-block",
  text: {
    bold: "composer-bold",
    italic: "composer-italic",
    strikethrough: "composer-strikethrough",
    underline: "composer-underline",
    code: "composer-inline-code",
  },
  link: "composer-link",
};

const initialConfig = {
  namespace: "CodexVoiceComposer",
  theme,
  nodes: [HeadingNode, QuoteNode, ListNode, ListItemNode, CodeNode, CodeHighlightNode, LinkNode],
  onError(error: Error) {
    throw error;
  },
};

function isEmotionTag(text: string): boolean {
  return /^\[[^\]\n]{1,80}\]$/.test(text);
}

/** Split bracketed delivery cues into styled text nodes without changing text content. */
function registerEmotionTagTransform(editor: LexicalEditor): () => void {
  return editor.registerNodeTransform(TextNode, (textNode) => {
    if ($isCodeNode(textNode.getParent()) || textNode.hasFormat("code")) return;
    const text = textNode.getTextContent();
    const matches = [...text.matchAll(/\[[^\]\n]{1,80}\]/g)];
    if (matches.length === 0) return;
    const offsets = [
      ...new Set(
        matches
          .flatMap((match) => [match.index ?? 0, (match.index ?? 0) + match[0].length])
          .filter((offset) => offset > 0 && offset < text.length),
      ),
    ];
    const parts = textNode.splitText(...offsets);
    for (const part of parts) {
      if (isEmotionTag(part.getTextContent()) && !part.getStyle().includes("--emotion-tag")) {
        part.setStyle(
          "--emotion-tag:1;font-weight:650;color:var(--emotion-ink);border:1px solid var(--emotion-border);padding-inline:0.2em;border-radius:0.38em;white-space:nowrap;",
        );
      }
    }
  });
}

interface EditorBridgeProps {
  textRef: MutableRefObject<TextMirrorElement | null>;
  value: string;
  onChange: (value: string) => void;
}

function EditorBridge({ textRef, value, onChange }: EditorBridgeProps) {
  const [editor] = useLexicalComposerContext();
  const lastAppliedValue = useRef(value);
  const changeTimer = useRef<number | null>(null);

  useEffect(() => registerEmotionTagTransform(editor), [editor]);

  useEffect(() => {
    if (value === lastAppliedValue.current) return;
    lastAppliedValue.current = value;
    editor.update(() => $convertFromMarkdownString(value, TRANSFORMERS));
  }, [editor, value]);

  useEffect(() => {
    const mirror = textRef.current;
    if (!mirror) return;
    Object.defineProperty(mirror, "value", {
      configurable: true,
      get: () => editor.getEditorState().read(() => $convertToMarkdownString(TRANSFORMERS)),
      set: (next: string) => {
        editor.update(() => $convertFromMarkdownString(String(next), TRANSFORMERS), {
          discrete: true,
        });
      },
    });
    return () => {
      if (changeTimer.current !== null) window.clearTimeout(changeTimer.current);
    };
  }, [editor, textRef]);

  const handleChange = (_editorState: EditorState): void => {
    if (changeTimer.current !== null) window.clearTimeout(changeTimer.current);
    changeTimer.current = window.setTimeout(() => {
      changeTimer.current = null;
      const markdown = editor.getEditorState().read(() => $convertToMarkdownString(TRANSFORMERS));
      lastAppliedValue.current = markdown;
      onChange(markdown);
    }, 120);
  };

  return <OnChangePlugin onChange={handleChange} ignoreSelectionChange />;
}

/** The Markdown-capable prompt editor with the existing paste and clear controls. */
export function TextEditor(props: TextEditorProps) {
  return (
    <div className="text-shell relative flex flex-auto min-h-[260px] overflow-hidden rounded-[22px] border border-[var(--line)] bg-[var(--panel)] p-0 [--text-button-clearance:126px] [--text-edge-pad:8px] focus-within:border-[var(--accent)] focus-within:shadow-[0_0_0_3px_var(--focus-ring)]">
      <LexicalComposer
        initialConfig={{
          ...initialConfig,
          editorState: () => $convertFromMarkdownString(props.value, TRANSFORMERS),
        }}
      >
        <RichTextPlugin
          contentEditable={
            <div className="h-full min-h-0 w-full flex-auto">
              <ContentEditable
                id="text"
                aria-label="Text to synthesize"
                data-placeholder="Type something to hear it spoken..."
                className="composer-content h-full min-h-0 w-full resize-none overflow-y-auto rounded-none border-0 bg-transparent px-4 pt-[var(--text-edge-pad)] pb-[calc(var(--text-button-clearance)_+_var(--text-edge-pad))] text-[0.94rem] leading-[1.45] text-[var(--text)] outline-none [scroll-padding:var(--text-edge-pad)_16px_calc(var(--text-button-clearance)_+_var(--text-edge-pad))]"
                onPaste={props.onPaste}
              />
            </div>
          }
          placeholder={null}
          ErrorBoundary={LexicalErrorBoundary}
        />
        <HistoryPlugin />
        <ListPlugin />
        <MarkdownShortcutPlugin transformers={TRANSFORMERS} />
        <EditorBridge textRef={props.textRef} value={props.value} onChange={props.onChange} />
      </LexicalComposer>
      <textarea
        ref={props.textRef}
        defaultValue={props.value}
        readOnly
        aria-hidden="true"
        tabIndex={-1}
        data-testid="composer-source"
        className="composer-source-mirror"
        onInput={(event) => props.onChange(event.currentTarget.value)}
      />
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
