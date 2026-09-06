import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { bracketMatching, indentOnInput } from "@codemirror/language";
import { yaml } from "@codemirror/lang-yaml";
import { oneDark } from "@codemirror/theme-one-dark";
import { lintGutter, setDiagnostics, type Diagnostic } from "@codemirror/lint";
import type { ScoreIssue } from "../types";

interface Props {
  value: string;
  onChange: (v: string) => void;
  errors: ScoreIssue[];
  onCompile: () => void;
  onLoad: () => void;
}

/** Thin CodeMirror 6 wrapper. The document is owned by CodeMirror; `value` is only
 *  applied when it differs (e.g. initial load from the backend). */
export function Editor({ value, onChange, errors, onCompile, onLoad }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const cbs = useRef({ onChange, onCompile, onLoad });
  cbs.current = { onChange, onCompile, onLoad };

  useEffect(() => {
    if (!host.current) return;
    const state = EditorState.create({
      doc: value,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightActiveLine(),
        drawSelection(),
        history(),
        indentOnInput(),
        bracketMatching(),
        lintGutter(),
        yaml(),
        oneDark,
        keymap.of([
          { key: "Ctrl-Enter", run: () => (cbs.current.onCompile(), true) },
          { key: "Ctrl-s", run: () => (cbs.current.onLoad(), true), preventDefault: true },
          indentWithTab,
          ...defaultKeymap,
          ...historyKeymap,
        ]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) cbs.current.onChange(u.state.doc.toString());
        }),
        EditorView.theme({
          "&": { height: "100%", fontSize: "14px" },
          ".cm-scroller": { fontFamily: "'JetBrains Mono', 'Cascadia Code', Consolas, monospace", overflow: "auto" },
        }),
      ],
    });
    const v = new EditorView({ state, parent: host.current });
    view.current = v;
    return () => {
      v.destroy();
      view.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // External value changes (initial load) -> replace document.
  useEffect(() => {
    const v = view.current;
    if (!v) return;
    const cur = v.state.doc.toString();
    if (cur !== value) {
      v.dispatch({ changes: { from: 0, to: cur.length, insert: value } });
    }
  }, [value]);

  // Errors -> inline diagnostics.
  useEffect(() => {
    const v = view.current;
    if (!v) return;
    const doc = v.state.doc;
    const diags: Diagnostic[] = errors.map((e) => {
      const lineNo = Math.min(Math.max(e.line ?? 1, 1), doc.lines);
      const line = doc.line(lineNo);
      let from = line.from;
      if (e.column && e.column > 0) from = Math.min(line.from + e.column - 1, line.to);
      const to = Math.max(from, line.to);
      return {
        from: from === to && line.length === 0 ? line.from : from,
        to,
        severity: "error",
        message: e.suggestion ? `${e.message}\n${e.suggestion}` : e.message,
      };
    });
    v.dispatch(setDiagnostics(v.state, diags));
  }, [errors]);

  return <div className="editor-host" ref={host} />;
}
