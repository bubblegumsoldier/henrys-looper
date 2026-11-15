//! The second view of the same document: the score as YAML, with a block preview and the
//! compiler's messages beside it (design/arrangement-text.html).
//!
//! The editor does not keep its own undo history: ⌘Z here walks the document history, the same one
//! the grid uses, so an undo never has two meanings depending on which view had the focus. Text
//! that arrives from the store (an undo, a grid edit) is merged in as the smallest change, so the
//! cursor stays where it was.

import { useEffect, useMemo, useRef } from "react";
import { Annotation, EditorState, Compartment, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  MatchDecorator,
  ViewPlugin,
  drawSelection,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { defaultKeymap, indentWithTab } from "@codemirror/commands";
import { HighlightStyle, bracketMatching, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { yaml } from "@codemirror/lang-yaml";
import { setDiagnostics, type Diagnostic } from "@codemirror/lint";
import { tags } from "@lezer/highlight";
import type { CompiledScore, ModelIssue, ScoreDoc } from "../types";
import { STATE_LOOK, plural } from "./labels";
import { resolveSection, sectionSpans } from "./resolve";

interface Props {
  text: string;
  /** Changes whenever the text was replaced from outside the editor. */
  textRev: number;
  onChange: (text: string) => void;
  onUndo: () => void;
  onRedo: () => void;
  readOnly: boolean;
  errors: ModelIssue[];
  /** Not errors: what does nothing yet (`loop: midi`, `grid`). */
  notes?: ModelIssue[];
  /** Model to preview: the text's, or the last one that parsed. */
  doc: ScoreDoc | null;
  inSync: boolean;
  score: CompiledScore | null;
  onCursor: (line: number, column: number) => void;
  /** Put the cursor on this line (from a message click). */
  jump: { line: number; seq: number } | null;
  onJump: (line: number) => void;
}

/** Marks a transaction whose text came from outside the editor, so it is not echoed back. */
const external = Annotation.define<boolean>();

const highlight = HighlightStyle.define([
  { tag: tags.definition(tags.propertyName), color: "#9fb4ff" },
  { tag: tags.lineComment, color: "#6b7686", fontStyle: "italic" },
  { tag: tags.string, color: "#e6c07b" },
  { tag: tags.separator, color: "#c9d1dc" },
  { tag: tags.meta, color: "#6b7686" },
]);

/** Values the grammar calls plain "content": numbers, booleans and the six states get colours. */
const VALUE = /(?<=[:[,]\s*)(true|false|-?\d+(?:\.\d+)?|record|overdub|stack|play|stop|hear_through)(?=\s*(?:[,}\]]|#|$))/gm;
const valueClass: Record<string, string> = {
  record: "y-rec",
  overdub: "y-dub",
  stack: "y-dub",
  play: "y-play",
  stop: "y-stop",
  hear_through: "y-hear",
  true: "y-b",
  false: "y-b",
};
const values = new MatchDecorator({
  regexp: VALUE,
  decoration: (m) => Decoration.mark({ class: valueClass[m[1]] ?? "y-n" }),
});
const valuePlugin = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = values.createDeco(view);
    }
    update(u: ViewUpdate) {
      this.decorations = values.updateDeco(u, this.decorations);
    }
  },
  { decorations: (v) => v.decorations },
);

const theme = EditorView.theme(
  {
    "&": { height: "100%", fontSize: "12.5px", backgroundColor: "var(--bg-0)", color: "#c9d1dc" },
    ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "20px", overflow: "auto" },
    ".cm-content": { padding: "10px 0", caretColor: "#fff" },
    ".cm-line": { padding: "0 16px" },
    ".cm-gutters": { backgroundColor: "var(--bg-1)", borderRight: "1px solid var(--line-2)", color: "var(--tx-4)" },
    ".cm-lineNumbers .cm-gutterElement": { padding: "0 10px 0 14px", minWidth: "46px" },
    ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--tx-1)" },
    ".cm-activeLine": { backgroundColor: "rgba(255,255,255,.035)" },
    "&.cm-focused .cm-cursor": { borderLeftColor: "#fff" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": { backgroundColor: "rgba(142,162,255,.25) !important" },
    ".cm-lintRange-error": {
      backgroundImage: "none",
      textDecoration: "underline wavy var(--err)",
      textUnderlineOffset: "4px",
      textDecorationThickness: "1.2px",
    },
    ".cm-tooltip": { border: "none", background: "transparent" },
    ".cm-tooltip-lint": {
      width: "360px",
      padding: "10px 12px",
      borderRadius: "8px",
      background: "#2a1518",
      border: "1px solid rgba(255,92,108,.55)",
      boxShadow: "var(--sh-pop)",
      font: "12px/1.45 var(--font-ui)",
      color: "#ffd6da",
      whiteSpace: "pre-wrap",
    },
    ".cm-diagnostic": { padding: 0, border: "none" },
    ".cm-diagnostic-error": { borderLeft: "none" },
  },
  { dark: true },
);

/** The smallest replacement that turns `a` into `b`, so a cursor outside it does not move. */
function diff(a: string, b: string) {
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--;
    endB--;
  }
  return { from: start, to: endA, insert: b.slice(start, endB) };
}

export function TextView(p: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const cbs = useRef(p);
  cbs.current = p;
  const ro = useRef(new Compartment());

  useEffect(() => {
    if (!host.current) return;
    const extensions: Extension[] = [
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightActiveLine(),
      drawSelection(),
      indentOnInput(),
      bracketMatching(),
      yaml(),
      syntaxHighlighting(highlight),
      valuePlugin,
      theme,
      ro.current.of([EditorState.readOnly.of(p.readOnly), EditorView.editable.of(!p.readOnly)]),
      keymap.of([
        { key: "Mod-z", run: () => (cbs.current.onUndo(), true), preventDefault: true },
        { key: "Mod-Shift-z", run: () => (cbs.current.onRedo(), true), preventDefault: true },
        { key: "Mod-y", run: () => (cbs.current.onRedo(), true), preventDefault: true },
        indentWithTab,
        ...defaultKeymap,
      ]),
      EditorView.updateListener.of((u) => {
        if (u.docChanged && !u.transactions.some((t) => t.annotation(external))) cbs.current.onChange(u.state.doc.toString());
        if (u.selectionSet || u.docChanged) {
          const head = u.state.selection.main.head;
          const line = u.state.doc.lineAt(head);
          cbs.current.onCursor(line.number, head - line.from + 1);
        }
      }),
    ];
    const v = new EditorView({ state: EditorState.create({ doc: p.text, extensions }), parent: host.current });
    view.current = v;
    return () => {
      v.destroy();
      view.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Text replaced from outside (undo, a grid edit, open).
  useEffect(() => {
    const v = view.current;
    if (!v) return;
    const cur = v.state.doc.toString();
    if (cur === p.text) return;
    const change = diff(cur, p.text);
    v.dispatch({ changes: change, annotations: external.of(true) });
  }, [p.textRev, p.text]);

  useEffect(() => {
    view.current?.dispatch({
      effects: ro.current.reconfigure([EditorState.readOnly.of(p.readOnly), EditorView.editable.of(!p.readOnly)]),
    });
  }, [p.readOnly]);

  // Errors -> squiggles. Only issues that belong to this text: when the text does not parse, the
  // errors are the parse errors of exactly this text.
  useEffect(() => {
    const v = view.current;
    if (!v) return;
    const doc = v.state.doc;
    const notes = p.notes ?? [];
    const all = [...p.errors.map((e) => ({ e, severity: "error" as const })), ...notes.map((e) => ({ e, severity: "warning" as const }))];
    const diags: Diagnostic[] = all.map(({ e, severity }) => {
      const lineNo = Math.min(Math.max(e.line ?? 1, 1), doc.lines);
      const line = doc.line(lineNo);
      let from = line.from;
      if (e.column && e.column > 0) from = Math.min(line.from + e.column - 1, line.to);
      // Underline the word under the column, not the rest of the line.
      const rest = doc.sliceString(from, line.to);
      const word = /^[^\s,}\]]+/.exec(rest);
      const to = word ? from + word[0].length : line.to;
      return {
        from: from === to && line.length === 0 ? line.from : from,
        to,
        severity,
        message: e.suggestion ? `${e.message}\n${e.suggestion}` : e.message,
      };
    });
    v.dispatch(setDiagnostics(v.state, diags));
  }, [p.errors, p.notes, p.text]);

  useEffect(() => {
    const v = view.current;
    if (!v || !p.jump) return;
    const line = v.state.doc.line(Math.min(Math.max(1, p.jump.line), v.state.doc.lines));
    v.dispatch({ selection: { anchor: line.from }, scrollIntoView: true });
    v.focus();
  }, [p.jump]);

  const errorLines = useMemo(() => new Set(p.errors.map((e) => e.line).filter((l): l is number => !!l)), [p.errors]);

  return (
    <div className="main">
      <div className="code-host" ref={host} data-error-lines={[...errorLines].join(",")} />
      <aside className="insp">
        <div className="insp-h">
          <div className="kicker">
            <span className="caps">Block preview</span>
            {p.errors.length > 0 ? (
              <span className="pill warn" style={{ height: 20 }}>
                <i className="led" />
                {plural(p.errors.length, "error")}
              </span>
            ) : (
              <span className="pill" style={{ height: 20 }}>
                <i className="led" />
                valid
              </span>
            )}
          </div>
          <div className="sub" style={{ marginTop: 6 }}>
            The score as the engine would read it.{" "}
            {p.inSync ? "" : "The last valid version stays active until the error is fixed."}
          </div>
        </div>
        <div className="insp-s">{p.doc ? <Preview doc={p.doc} errors={p.errors} /> : <div className="hint">Nothing to preview yet.</div>}</div>
        <div className="insp-s">
          <span className="caps">Messages</span>
          {p.errors.map((e, i) => (
            <button key={i} className="diag err" onClick={() => e.line && p.onJump(e.line)} title="Go to the line">
              <i className="ic ic-warn" />
              <div>
                {e.line ? <b>Line {e.line}: </b> : null}
                {e.message}
                {e.suggestion && <div className="sugg">{e.suggestion}</div>}
              </div>
            </button>
          ))}
          {(p.notes ?? []).map((e, i) => (
            <button key={`n${i}`} className="diag" onClick={() => e.line && p.onJump(e.line)} title="Go to the line">
              <i className="ic ic-info" />
              <div>
                {e.line ? <b>Line {e.line}: </b> : null}
                {e.message}
              </div>
            </button>
          ))}
          {p.doc && <Notes doc={p.doc} />}
        </div>
      </aside>
    </div>
  );
}

function Preview({ doc, errors }: { doc: ScoreDoc; errors: ModelIssue[] }) {
  const spans = sectionSpans(doc);
  const bad = new Set(errors.flatMap((e) => (e.at?.kind === "section_track" ? [`${e.at.section}:${e.at.track}`] : [])));
  return (
    <>
      <div className="pv">
        <div className="pv-r pv-h">
          <span />
          {doc.sections.map((s, i) => (
            <span key={i} style={{ flex: Math.max(2, spans[i].bars), textAlign: "center" }} title={s.id}>
              {i + 1}
            </span>
          ))}
        </div>
        {doc.tracks.map((t, ti) => (
          <div key={ti} className="pv-r">
            <span className="pv-t" style={{ "--c": `var(--trk-${(ti % 8) + 1})` } as React.CSSProperties}>
              {t.name}
            </span>
            {doc.sections.map((s, si) => {
              const st = (s.resolved ?? resolveSection(doc, si)).tracks[ti]?.state ?? "stop";
              const look = STATE_LOOK[st];
              const err = bad.has(`${si}:${t.name}`);
              return (
                <span
                  key={si}
                  className={`pv-c${err ? " err" : ""}`}
                  style={{ flex: Math.max(2, spans[si].bars), "--k": st === "stop" ? "var(--bg-4)" : `var(--st-${look.cls})` } as React.CSSProperties}
                  title={`${t.name} · ${s.id}: ${look.word}`}
                >
                  <i className={`ic ${err ? "ic-warn" : look.icon}`} />
                </span>
              );
            })}
          </div>
        ))}
      </div>
      <div className="hint">
        Columns = sections 1–{doc.sections.length} (width by bars), rows = tracks.
      </div>
    </>
  );
}

/** A few facts worth knowing that are not errors - the mockup's info lines. */
function Notes({ doc }: { doc: ScoreDoc }) {
  const notes: { line?: number; text: string }[] = [];
  doc.sections.forEach((s, i) => {
    if (s.repeat === undefined) return;
    const r = s.resolved ?? resolveSection(doc, i);
    const inherited = r.tracks.filter((t) => t.from.kind === "repeat").length;
    const what = [r.bars.from.kind === "repeat" ? `${r.bars.value} bars` : null, inherited ? (inherited === doc.tracks.length ? "all states" : plural(inherited, "state")) : null]
      .filter(Boolean)
      .join(" and ");
    if (what) notes.push({ line: s.line, text: `${s.id} inherits ${what} from ${s.repeat}.` });
  });
  doc.tracks.forEach((t) => {
    if (t.instrument)
      notes.push({
        line: t.line,
        text: `${t.name} is an instrument track: played over MIDI, always stereo, heard whenever it is played${t.monitor === false ? " (here: never, monitor: false)" : ""}.`,
      });
    else if (t.input_right) notes.push({ line: t.line, text: `${t.name} is stereo (inputs ${t.input} and ${t.input_right}).` });
  });
  if (doc.mixer) {
    const n = doc.mixer.tracks.length;
    notes.push({
      line: doc.mixer.tracks[0]?.line,
      text: `mixer: faders, mutes and effects of ${plural(n, "track")}${doc.mixer.buses ? " and the bus levels" : ""} are kept with the song. While this score is loaded the section follows the mixer, and the mixer follows the section.`,
    });
  }
  return (
    <>
      {notes.map((n, i) => (
        <div key={i} className="diag">
          <i className="ic ic-info" />
          <div>
            {n.line ? <b>Line {n.line}: </b> : null}
            {n.text}
          </div>
        </div>
      ))}
    </>
  );
}
