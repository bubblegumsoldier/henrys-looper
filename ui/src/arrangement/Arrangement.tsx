//! The arrangement editor: one document, two views (Grid | Text), an inspector, a status bar.
//! Layout and look follow design/arrangement.html and design/arrangement-text.html.
//!
//! ```text
//!  Arrangement ── useDoc() ── docStore (text + model + history)      ── useRun() ── status hub
//!     ├─ toolbar      view switch, section/track actions, zoom, inspector toggle
//!     ├─ Grid         ruler + heads + lanes            ┐ both edit through docStore.edit(op)
//!     ├─ Inspector    the selection's fields            ┘
//!     ├─ TextView     CodeMirror + block preview + messages (docStore.setText)
//!     └─ statusbar
//! ```
//!
//! While a score plays the arrangement is locked: the runner holds the text that was loaded, and an
//! edit now would only make the picture disagree with what is sounding.

import { useCallback, useEffect, useMemo, useState } from "react";
import { MOD_LABEL } from "../platform";
import type { ModelIssue, ScoreDoc } from "../types";
import { Grid } from "./Grid";
import { Inspector } from "./Inspector";
import { STATE_BY_KEY, duration, plural } from "./labels";
import * as ops from "./ops";
import { secondsPerBar, totalBars } from "./resolve";
import { NONE, moveFocus, prune, type Selection } from "./selection";
import { TextView } from "./TextView";
import type { ScoreActions } from "./scoreActions";
import { useScoreFile } from "../scoreFile";
import { docStore, useDoc, useEngineTracks, useRun } from "./useDoc";
import "./arrangement.css";

type View = "grid" | "text";

const PHASE_WORD: Record<string, string> = {
  none: "no score loaded",
  idle: "loaded, ready",
  count_in: "count-in",
  running: "playing",
  finished: "finished",
  stopped: "stopped",
};

function stored<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}
function store(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* a convenience only */
  }
}

function typing(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable || el.closest(".cm-editor") !== null;
}

interface Props {
  /** Jump the running score to a section (0-based). */
  onGoto: (section: number) => void;
  actions: ScoreActions;
}

/** New, Open, recent files, Save, Save as - one menu, so the toolbar keeps the mockup's order. */
function FileMenu({ actions }: { actions: ScoreActions }) {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => {
      if (!(e.target as HTMLElement).closest?.(".filemenu")) setOpen(false);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [open]);
  const act = (f: () => Promise<void>) => () => {
    setOpen(false);
    void f();
  };
  return (
    <div className="filemenu">
      <button className={`btn s${open ? " on" : ""}`} onClick={() => setOpen(!open)} title="Score file">
        <i className="ic ic-folder" />
        File
        <i className="ic ic-chev-d" />
      </button>
      {open && (
        <div className="menu">
          <button onClick={act(actions.newScore)}>
            New<kbd>{MOD_LABEL}N</kbd>
          </button>
          <button onClick={act(actions.openFile)}>
            Open…<kbd>{MOD_LABEL}O</kbd>
          </button>
          <button onClick={act(actions.save)}>
            Save<kbd>{MOD_LABEL}S</kbd>
          </button>
          <button onClick={act(actions.saveAs)}>
            Save as…<kbd>⇧{MOD_LABEL}S</kbd>
          </button>
          {actions.recent.length > 0 && <div className="menu-h">Recent</div>}
          {actions.recent.map((r) => (
            <button key={r.path} disabled={!r.exists} title={r.path} onClick={act(() => actions.openRecent(r.path))}>
              {r.name}
              {r.exists ? "" : " (missing)"}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** Count-in and Load: hands the score to the runner. */
function LoadControls({ actions, ok, locked }: { actions: ScoreActions; ok: boolean; locked: boolean }) {
  return (
    <>
      <span className="lbl" title="Bars of click before the first section (1 to 8)">
        Count-in
      </span>
      <span className="field countin">
        <input
          type="number"
          min={1}
          max={8}
          value={actions.countIn}
          disabled={locked}
          onChange={(e) => actions.setCountIn(Number(e.target.value))}
          title="Bars of click before the first section (1 to 8)"
        />
      </span>
      <button
        className={`btn s${actions.stale ? " accent" : ""}`}
        disabled={!ok || locked || actions.loading}
        onClick={() => void actions.load()}
        title={
          actions.stale
            ? "The score changed since it was loaded — load it again. Needs a running engine with empty tracks."
            : "Load the score into the runner. Needs a running engine with empty tracks."
        }
      >
        <i className="ic ic-target" />
        {actions.stale ? "Reload" : "Load"}
      </button>
    </>
  );
}

export function Arrangement({ onGoto, actions }: Props) {
  const fileName = useScoreFile().name;
  const state = useDoc();
  const run = useRun();
  const engineTracks = useEngineTracks();
  const [view, setViewRaw] = useState<View>(() => stored("arr.view", "grid"));
  const [zoom, setZoomRaw] = useState<number>(() => stored("arr.zoom", 20));
  const [inspector, setInspectorRaw] = useState<boolean>(() => stored("arr.insp", true));
  const [selRaw, setSel] = useState<Selection>(NONE);
  const [cursor, setCursor] = useState({ line: 1, column: 1 });
  const [jump, setJump] = useState<{ line: number; seq: number } | null>(null);

  const setView = (v: View) => {
    setViewRaw(v);
    store("arr.view", v);
  };
  const setZoom = (z: number) => {
    const v = Math.max(6, Math.min(80, Math.round(z)));
    setZoomRaw(v);
    store("arr.zoom", v);
  };
  const setInspector = (v: boolean) => {
    setInspectorRaw(v);
    store("arr.insp", v);
  };

  const doc = state.doc;
  const sel = useMemo(() => prune(selRaw, doc), [selRaw, doc]);
  const locked = run.playing;
  const editable = !locked && state.inSync && doc !== null;

  const edit = useCallback(
    (op: (d: ScoreDoc) => ScoreDoc) => {
      if (locked) return;
      docStore.edit(op);
    },
    [locked],
  );

  const showIssue = useCallback((issue: ModelIssue) => {
    if (!issue.line) return;
    setView("text");
    setJump({ line: issue.line, seq: Date.now() });
  }, []);

  // --- toolbar actions ---------------------------------------------------------------------------
  const selSection = sel.kind === "section" ? sel.index : sel.kind === "cells" ? sel.focus.section : null;
  const addSection = () => {
    if (!doc) return;
    const at = selSection === null ? doc.sections.length : selSection + 1;
    edit((d) => ops.addSection(d, at));
    setSel({ kind: "section", index: at });
  };
  const duplicate = () => {
    if (selSection === null) return;
    edit((d) => ops.duplicateSection(d, selSection));
    setSel({ kind: "section", index: selSection + 1 });
  };
  const moveSel = (by: number) => {
    if (selSection === null || !doc) return;
    const to = selSection + by;
    if (to < 0 || to >= doc.sections.length) return;
    edit((d) => ops.moveSection(d, selSection, to));
    setSel({ kind: "section", index: to });
  };
  const deleteSel = () => {
    if (!doc) return;
    if (sel.kind === "track") {
      if (doc.tracks.length <= 1) return;
      edit((d) => ops.deleteTrack(d, sel.index));
      setSel(NONE);
    } else if (sel.kind === "cells") {
      edit((d) => ops.resetCells(d, sel.cells));
    } else if (selSection !== null && doc.sections.length > 1) {
      edit((d) => ops.deleteSection(d, selSection));
      setSel(NONE);
    }
  };
  const addTrack = () => {
    if (!doc) return;
    edit((d) => ops.addTrack(d));
    setSel({ kind: "track", index: doc.tracks.length });
  };

  // --- keyboard ----------------------------------------------------------------------------------
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (typing(e.target)) return;
      const mod = e.metaKey || e.ctrlKey;
      const key = e.key.toLowerCase();
      if (mod && key === "z") {
        e.preventDefault();
        if (locked) return;
        if (e.shiftKey) docStore.redo();
        else docStore.undo();
        return;
      }
      if (mod && key === "y") {
        e.preventDefault();
        if (!locked) docStore.redo();
        return;
      }
      if (view !== "grid" || !doc) return;
      if (mod && key === "d") {
        e.preventDefault();
        if (editable) duplicate();
        return;
      }
      if (mod || e.altKey) return;
      if (key === "+" || key === "=") return setZoom(zoom * 1.25);
      if (key === "-") return setZoom(zoom / 1.25);
      if (key === "escape") return setSel(NONE);
      const arrows: Record<string, [number, number]> = { arrowleft: [-1, 0], arrowright: [1, 0], arrowup: [0, -1], arrowdown: [0, 1] };
      if (arrows[key]) {
        e.preventDefault();
        setSel(moveFocus(sel, arrows[key][0], arrows[key][1], e.shiftKey, doc));
        return;
      }
      if (!editable) return;
      if ((key === "backspace" || key === "delete") && sel.kind !== "score") {
        e.preventDefault();
        deleteSel();
        return;
      }
      const state = STATE_BY_KEY[key];
      if (state && !e.shiftKey && sel.kind === "cells") {
        e.preventDefault();
        edit((d) => ops.setCellState(d, sel.cells, state));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // --- render ------------------------------------------------------------------------------------
  const bars = doc ? totalBars(doc) : 0;
  const errors = state.errors;
  const validity =
    errors.length === 0 ? (
      <span className="ok">
        <i className="ic ic-check" /> Score valid
      </span>
    ) : (
      <button className="warnlink" onClick={() => (view === "grid" ? setSel(NONE) : errors[0] && showIssue(errors[0]))}>
        <i className="ic ic-warn" /> {plural(errors.length, "problem")}
        {state.inSync ? "" : " · last valid version shown"}
      </button>
    );

  const banner = locked ? (
    <span className="lockpill" title="While the score plays, the arrangement cannot be edited. Click a section marker to jump there.">
      <i className="ic ic-lock" />
      Playing — arrangement locked
    </span>
  ) : !state.inSync && doc ? (
    <button className="lockpill warn" onClick={() => setView("text")} title="The text has errors. The grid shows the last valid version and is read-only until the text is fixed (or undone).">
      <i className="ic ic-warn" />
      Text has errors — grid read-only
    </button>
  ) : null;

  return (
    <div className="hl-arr">
      <div className="workspace">
        <div className="toolbar">
          <div className="seg">
            <button className={view === "grid" ? "on" : undefined} onClick={() => setView("grid")}>
              <i className="ic ic-grid" />
              Grid
            </button>
            <button className={view === "text" ? "on" : undefined} onClick={() => setView("text")}>
              <i className="ic ic-code" />
              Text (YAML)
            </button>
          </div>
          <div className="sep" />
          {view === "grid" ? (
            <>
              <button className="btn s" disabled={!editable} onClick={addSection} title="Add a section after the selected one">
                <i className="ic ic-plus" />
                Section
              </button>
              <button className="btn s" disabled={!editable || selSection === null} onClick={duplicate} title={`Duplicate the section (${MOD_LABEL}+D)`}>
                <i className="ic ic-dup" />
                Duplicate
              </button>
              <button className="btn s sq" disabled={!editable || selSection === null || selSection === 0} onClick={() => moveSel(-1)} title="Move section left">
                <i className="ic ic-arr-l" />
              </button>
              <button
                className="btn s sq"
                disabled={!editable || selSection === null || !doc || selSection >= doc.sections.length - 1}
                onClick={() => moveSel(1)}
                title="Move section right"
              >
                <i className="ic ic-arr-r" />
              </button>
              <button
                className="btn s danger"
                disabled={!editable || sel.kind === "score" || sel.kind === "cells"}
                onClick={deleteSel}
                title="Delete the selected section or track"
              >
                <i className="ic ic-trash" />
                Delete
              </button>
              <div className="sep" />
              <button className="btn s" disabled={!editable || !doc || doc.tracks.length >= ops.MAX_TRACKS} onClick={addTrack}>
                <i className="ic ic-plus" />
                Track
              </button>
              <div className="sep" />
              <button className="btn s sq ghost" disabled={locked || !state.canUndo} onClick={() => docStore.undo()} title={`Undo (${MOD_LABEL}+Z)`}>
                <i className="ic ic-undo" />
              </button>
              <button className="btn s sq ghost" disabled={locked || !state.canRedo} onClick={() => docStore.redo()} title={`Redo (${MOD_LABEL}+Shift+Z)`}>
                <i className="ic ic-redo" />
              </button>
              {banner}
              <div className="grow" />
              <span className="lbl">Zoom</span>
              <button className="btn s sq ghost" onClick={() => setZoom(zoom / 1.25)} title="Zoom out (−)">
                <i className="ic ic-minus" />
              </button>
              <input
                className="zoom"
                type="range"
                min={6}
                max={80}
                value={zoom}
                onChange={(e) => setZoom(Number(e.target.value))}
                title={`${zoom} px per bar`}
              />
              <button className="btn s sq ghost" onClick={() => setZoom(zoom * 1.25)} title="Zoom in (+)">
                <i className="ic ic-plus" />
              </button>
              <div className="sep" />
              <FileMenu actions={actions} />
              <LoadControls actions={actions} ok={state.inSync && errors.length === 0 && !!doc} locked={locked} />
              <div className="sep" />
              <button className={`btn s${inspector ? " on" : ""}`} onClick={() => setInspector(!inspector)}>
                <i className="ic ic-split" />
                Inspector
              </button>
            </>
          ) : (
            <>
              <span className="dim" style={{ fontSize: 11 }}>
                {fileName ?? "Untitled"}
              </span>
              {banner}
              <div className="grow" />
              <span className="lbl">Both views edit the same score — changes show up in the grid at once.</span>
              <div className="sep" />
              <FileMenu actions={actions} />
              <button className="btn s" onClick={() => void actions.save()} title={`Save (${MOD_LABEL}+S)`}>
                <i className="ic ic-save" />
                Save
              </button>
              <LoadControls actions={actions} ok={state.inSync && errors.length === 0 && !!doc} locked={locked} />
            </>
          )}
        </div>

        {view === "grid" ? (
          <div className={`main${inspector ? "" : " no-insp"}`}>
            <div className="editor">
              {doc ? (
                <Grid
                  doc={doc}
                  errors={state.inSync ? errors : []}
                  editable={editable}
                  run={run}
                  sel={sel}
                  setSel={setSel}
                  zoom={zoom}
                  edit={edit}
                  onGoto={onGoto}
                  engineTracks={engineTracks}
                />
              ) : (
                <div className="arr-empty">
                  {state.failure ? (
                    <>
                      <b>The score cannot be read here.</b>
                      <span>{state.failure}</span>
                    </>
                  ) : state.text.trim() === "" ? (
                    <>
                      <b>No score yet.</b>
                      <span>Use New to start one from a template, or Open a score file.</span>
                    </>
                  ) : (
                    <>
                      <b>This text does not parse yet.</b>
                      <button className="btn s" onClick={() => setView("text")}>
                        Show the text and its errors
                      </button>
                    </>
                  )}
                </div>
              )}
            </div>
            {inspector && doc && (
              <Inspector doc={doc} errors={state.inSync ? errors : []} editable={editable} sel={sel} setSel={setSel} edit={edit} onIssue={showIssue} />
            )}
          </div>
        ) : (
          <TextView
            text={state.text}
            textRev={state.textRev}
            onChange={(t) => docStore.setText(t)}
            onUndo={() => !locked && docStore.undo()}
            onRedo={() => !locked && docStore.redo()}
            readOnly={locked}
            errors={errors}
            doc={doc}
            inSync={state.inSync}
            score={state.score}
            onCursor={(line, column) => setCursor({ line, column })}
            jump={jump}
            onJump={(line) => setJump({ line, seq: Date.now() })}
          />
        )}

        <div className="statusbar">
          {view === "grid" ? (
            <>
              <span>
                {doc ? `${plural(doc.sections.length, "section")} · ${plural(bars, "bar")} · ≈ ${duration(bars * secondsPerBar(doc))} without release loops` : "–"}
              </span>
              <span>
                Transport: {PHASE_WORD[run.phase] ?? run.phase}
                {run.playing && run.section !== null && doc?.sections[run.section] ? ` · ${doc.sections[run.section].id}, bar ${run.bar}, pass ${run.pass}` : ""}
              </span>
              {run.armed && run.armedText && <span className="armedtxt">{run.armedText}</span>}
            </>
          ) : (
            <>
              <span>
                Line {cursor.line}, Column {cursor.column}
              </span>
              <span>YAML · UTF-8</span>
            </>
          )}
          <div className="grow" />
          {validity}
          <span>{run.engine ? `Engine running · ${Math.round(run.sampleRate / 100) / 10} kHz` : "Engine off"}</span>
        </div>
      </div>
    </div>
  );
}
