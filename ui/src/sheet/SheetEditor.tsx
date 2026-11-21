//! The sheet in the Arrangement: a plain text editor on the left, the same page the Live view shows
//! on the right (`SheetView`).
//!
//! The text is the document's `sheet` field, and an edit goes into the score like every other
//! edit (`docStore.edit`: one undo step, the file is changed, Save keeps it). Typing is collected
//! for a moment first, so a burst of typing is one step and one render; the preview follows from
//! the engine's parse of the rendered score (`score.sheet`) - the one parser of the format.
//! An empty editor removes `sheet:` from the score.

import { useEffect, useRef, useState, type CSSProperties } from "react";
import type { CompiledScore, ModelIssue, ScoreDoc } from "../types";
import { SheetView } from "./SheetView";
import { useSheetLayout } from "./layout";
import "./sheet.css";

/** Pause in typing before the text goes into the score. */
const COMMIT_DELAY_MS = 400;

/** The issues that belong to the sheet: the compiler places them on lines of the block, which map
 *  to the `sheet` key. */
export function sheetIssues(issues: ModelIssue[]): ModelIssue[] {
  return issues.filter((i) => i.at?.kind === "score" && i.at.field === "sheet");
}

/** 1-based line of the `sheet:` key in the score text, or null. */
export function sheetKeyLine(text: string): number | null {
  const lines = text.split("\n");
  const at = lines.findIndex((l) => /^sheet\s*:/.test(l));
  return at < 0 ? null : at + 1;
}

interface Props {
  doc: ScoreDoc | null;
  /** The text of the whole score, to turn the compiler's file lines into sheet lines. */
  text: string;
  score: CompiledScore | null;
  errors: ModelIssue[];
  notes: ModelIssue[];
  editable: boolean;
  /** Apply an edit to the document; false when it was refused (try again later). */
  edit: (op: (d: ScoreDoc) => ScoreDoc) => boolean;
}

export function SheetEditor({ doc, text, score, errors, notes, editable, edit }: Props) {
  const stored = doc?.sheet ?? "";
  const [local, setLocal] = useState(stored);
  const pending = useRef<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const area = useRef<HTMLTextAreaElement | null>(null);
  const layout = useSheetLayout();

  // Follow the document (undo, the text view) unless typing here is still on its way.
  useEffect(() => {
    if (pending.current === null) setLocal(stored);
  }, [stored]);

  const commit = () => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    const value = pending.current;
    if (value === null) return;
    const ok = edit((d) => {
      const next = { ...d };
      if (value === "") delete next.sheet;
      else next.sheet = value;
      return (d.sheet ?? "") === value ? d : next;
    });
    if (ok) pending.current = null;
    else timer.current = window.setTimeout(commit, COMMIT_DELAY_MS);
  };
  // Whatever is typed is kept when the view is left.
  const commitRef = useRef(commit);
  commitRef.current = commit;
  useEffect(() => () => commitRef.current(), []);

  const change = (value: string) => {
    setLocal(value);
    pending.current = value;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(commit, COMMIT_DELAY_MS);
  };

  /** Put the cursor on a line of the sheet. */
  const goLine = (line: number) => {
    const el = area.current;
    if (!el) return;
    const lines = el.value.split("\n");
    const at = lines.slice(0, Math.max(0, line - 1)).reduce((n, l) => n + l.length + 1, 0);
    el.focus();
    el.setSelectionRange(at, at + (lines[line - 1]?.length ?? 0));
    const lh = parseFloat(getComputedStyle(el).lineHeight) || 19;
    el.scrollTop = Math.max(0, (line - 4) * lh);
  };

  const key = sheetKeyLine(text);
  const issues = [...sheetIssues(errors).map((i) => ({ i, err: true })), ...sheetIssues(notes).map((i) => ({ i, err: false }))];
  const sheet = score?.sheet;

  return (
    <div className="sheet-edit">
      <section className="src">
        <div className="cap">
          <code># Part</code> · <code># Part @section_id</code> ties it to a section · <code>&gt; stage direction</code> (starting
          with REC = red) · chords above the words, at their columns, or inline as <code>[Am]</code> · <code>|2</code> at the end = this
          line lasts 2 bars · <code>beats per chord: 2</code>
        </div>
        <textarea
          ref={area}
          value={local}
          spellCheck={false}
          disabled={!editable}
          placeholder={"# Verse 1 @verse_1\n> REC bass · 2 bars |2\nAm            C/G\nFirst line of the verse\n[F]Second line with [C]inline chords |1.5"}
          onChange={(e) => change(e.target.value)}
          onBlur={commit}
          aria-label="Sheet text"
        />
        {!editable && (
          <div className="cap">
            {doc ? "The arrangement is locked while the score plays, or the YAML text has errors." : "No score open."}
          </div>
        )}
        {issues.length > 0 && (
          <div className="issues">
            {issues.map(({ i, err }, n) => (
              <button
                key={n}
                className={err ? "err" : "note"}
                onClick={() => i.line && key !== null && goLine(i.line - key)}
                title={i.suggestion ?? undefined}
              >
                {i.line && key !== null ? `Line ${i.line - key}: ` : ""}
                {i.message.replace(/^Sheet: /, "")}
                {i.suggestion ? ` — ${i.suggestion}` : ""}
              </button>
            ))}
          </div>
        )}
      </section>
      <div className="sheet-preview" style={{ "--sheet-fs": `${layout.fontSize}px` } as CSSProperties}>
        {sheet && sheet.items.length > 0 ? (
          <SheetView sheet={sheet} onLine={goLine} />
        ) : (
          <div className="sheet-preview-empty">
            {local.trim() === ""
              ? "No sheet yet. Write or paste the lyrics with their chords on the left; the Live view shows them beside the stage (key L)."
              : "The preview follows once the score is valid."}
          </div>
        )}
      </div>
    </div>
  );
}
