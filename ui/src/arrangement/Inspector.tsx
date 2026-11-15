//! The inspector: every field of whatever is selected, with a plain sentence of what it will do
//! when the score runs.
//!
//! Four faces, one per kind of selection: the song (nothing selected), a section, one or more
//! clips (shown as their section, with the clip's track highlighted, as in the mockup), a track.

import { useEffect, useState, type ReactNode } from "react";
import { midiApi } from "../api";
import { InstrumentPicker } from "../mixer/InstrumentSlot";
import { noteName } from "../plugins/instruments";
import { usePlugins } from "../plugins/store";
import { T } from "../shell/terms";
import type { DocTrack, ModelIssue, ScoreDoc, ScoreState, TrackMidi } from "../types";
import { GRID_CHOICES, TransposeControl } from "../live/MidiLoopRows";
import { inputLabel } from "./Grid";
import { STATES, STATE_LOOK, busHas, busWord, duration, plural } from "./labels";
import * as ops from "./ops";
import { beatsPerBar, deriveLayers, resolveSection, secondsPerBar, sectionSpans, totalBars } from "./resolve";
import type { Selection } from "./selection";

interface Props {
  doc: ScoreDoc;
  errors: ModelIssue[];
  /** Not errors: what is written correctly and does nothing yet. */
  notes?: ModelIssue[];
  editable: boolean;
  sel: Selection;
  setSel: (s: Selection) => void;
  edit: (op: (d: ScoreDoc) => ScoreDoc) => void;
  onIssue: (issue: ModelIssue) => void;
}

export function Inspector(p: Props) {
  const { sel } = p;
  let body: ReactNode;
  if (sel.kind === "section" && p.doc.sections[sel.index]) body = <SectionPanel {...p} index={sel.index} />;
  else if (sel.kind === "cells" && sel.cells.length > 0) body = <SectionPanel {...p} index={sel.focus.section} />;
  else if (sel.kind === "track" && p.doc.tracks[sel.index]) body = <TrackPanel {...p} index={sel.index} />;
  else body = <ScorePanel {...p} />;
  return <aside className={`insp${p.editable ? "" : " readonly"}`}>{body}</aside>;
}

// ---------------------------------------------------------------------------------------------
// Small controls
// ---------------------------------------------------------------------------------------------

/** A text or number field that commits on Enter or when it loses focus. */
function Field(props: {
  value: string;
  onCommit: (v: string) => void;
  disabled?: boolean;
  width?: number;
  unit?: string;
  mono?: boolean;
  check?: (v: string) => string | null;
  placeholder?: string;
  big?: boolean;
}) {
  const [text, setText] = useState(props.value);
  useEffect(() => setText(props.value), [props.value]);
  const problem = props.check && text !== props.value ? props.check(text) : null;
  const commit = () => {
    if (text === props.value) return;
    if (problem) {
      setText(props.value);
      return;
    }
    props.onCommit(text);
  };
  return (
    <span className={`field${problem ? " bad" : ""}${props.big ? " big" : ""}`} style={props.width ? { width: props.width } : { flex: 1 }} title={problem ?? undefined}>
      <input
        className={props.mono ? "mono" : undefined}
        value={text}
        disabled={props.disabled}
        placeholder={props.placeholder}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
          if (e.key === "Escape") {
            setText(props.value);
            (e.target as HTMLInputElement).blur();
          }
        }}
      />
      {props.unit && <span className="unit">{props.unit}</span>}
    </span>
  );
}

function Seg<T extends string | boolean>(props: { value: T; options: { value: T; icon?: string; label: string }[]; onPick: (v: T) => void; disabled?: boolean }) {
  return (
    <div className={`seg${props.disabled ? " off" : ""}`}>
      {props.options.map((o) => (
        <button key={String(o.value)} className={o.value === props.value ? "on" : undefined} disabled={props.disabled} onClick={() => o.value !== props.value && props.onPick(o.value)}>
          {o.icon && <i className={`ic ${o.icon}`} />}
          {o.label}
        </button>
      ))}
    </div>
  );
}

function Toggle({ on, onChange, disabled, label }: { on: boolean; onChange: (v: boolean) => void; disabled?: boolean; label?: string }) {
  return (
    <button className="toggle-row" disabled={disabled} onClick={() => onChange(!on)}>
      <span className={`toggle${on ? " on" : ""}`} />
      {label && <span>{label}</span>}
    </button>
  );
}

function Problems({ errors, onIssue, filter }: { errors: ModelIssue[]; onIssue: (i: ModelIssue) => void; filter?: (i: ModelIssue) => boolean }) {
  const list = filter ? errors.filter(filter) : errors;
  if (list.length === 0) return null;
  return (
    <div className="insp-s problems">
      <span className="caps" style={{ color: "var(--err)" }}>
        <i className="ic ic-warn" /> {plural(list.length, "problem")}
      </span>
      {list.map((e, i) => (
        <button key={i} className="diag err" onClick={() => onIssue(e)} title="Show in text">
          <i className="ic ic-warn" />
          <div>
            {e.line ? <b>Line {e.line}: </b> : null}
            {e.message}
            {e.suggestion && <div className="sugg">{e.suggestion}</div>}
          </div>
        </button>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The song
// ---------------------------------------------------------------------------------------------

function ScorePanel({ doc, errors, editable, edit, setSel, onIssue }: Props) {
  const bars = totalBars(doc);
  const seconds = bars * secondsPerBar(doc);
  return (
    <>
      <div className="insp-h">
        <div className="kicker">
          <span className="caps">Song</span>
        </div>
        <h2>{doc.title || <span className="dim">Untitled</span>}</h2>
        <div className="sub">
          {plural(doc.sections.length, "section")} · {plural(bars, "bar")} · ≈ {duration(seconds)} without release loops
        </div>
      </div>
      <Problems errors={errors} onIssue={onIssue} />
      <div className="insp-s">
        <span className="caps">Song</span>
        <div className="frow">
          <label>Title</label>
          <div>
            <Field value={doc.title ?? ""} disabled={!editable} placeholder="Untitled" onCommit={(v) => edit((d) => ops.setTitle(d, v))} />
          </div>
        </div>
        <div className="frow">
          <label>Tempo</label>
          <div>
            <Field
              value={String(doc.bpm)}
              width={90}
              unit="BPM"
              disabled={!editable}
              check={(v) => (Number(v) >= 20 && Number(v) <= 400 ? null : "20 to 400 BPM")}
              onCommit={(v) => edit((d) => ops.setBpm(d, Number(v)))}
            />
          </div>
        </div>
        <div className="frow">
          <label>Time signature</label>
          <div>
            <Field
              value={doc.time_signature ?? `${beatsPerBar(doc)}/4`}
              width={90}
              disabled={!editable}
              check={(v) => (/^\s*\d{1,2}\s*\/\s*(1|2|4|8|16)\s*$/.test(v) ? null : "e.g. 3/4, 4/4, 6/8")}
              onCommit={(v) => edit((d) => ops.setTimeSignature(d, v.replace(/\s+/g, "")))}
            />
          </div>
        </div>
        <div className="hint">
          One bar takes {secondsPerBar(doc).toFixed(2).replace(/\.?0+$/, "")} s. Loading the score sets the engine's tempo and time
          signature to these values.
        </div>
      </div>
      <div className="insp-s">
        <span className="caps">Tracks</span>
        {doc.tracks.map((t, i) => (
          <button key={i} className="trow" onClick={() => setSel({ kind: "track", index: i })}>
            <i className="c" style={{ "--c": `var(--trk-${(i % 8) + 1})` } as React.CSSProperties} />
            <b>{t.name}</b>
            <span className="dim">{inputLabel(t)}</span>
            <span className="dim">{t.instrument || t.input_right ? "stereo" : "mono"}</span>
          </button>
        ))}
      </div>
      <div className="insp-s">
        <span className="caps">Keys</span>
        <div className="hint keys">
          <span><kbd>R</kbd> <kbd>O</kbd> <kbd>P</kbd> <kbd>S</kbd> <kbd>M</kbd> set the selected clips</span>
          <span><kbd>←</kbd><kbd>→</kbd><kbd>↑</kbd><kbd>↓</kbd> move, with <kbd>⇧</kbd> select a range</span>
          <span><kbd>⌫</kbd> reset clips to the template, or delete a section/track</span>
          <span><kbd>⌘D</kbd> duplicate section · <kbd>⌘Z</kbd> undo · <kbd>⇧⌘Z</kbd> redo</span>
        </div>
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------------------------
// A section (and the clips in it)
// ---------------------------------------------------------------------------------------------

function InhTag({ from }: { from: { kind: string; section?: string } }) {
  if (from.kind === "repeat")
    return (
      <span className="inh-tag">
        <i className="ic ic-link" />
        from {from.section}
      </span>
    );
  if (from.kind === "default") return <span className="inh-tag dflt">default</span>;
  return null;
}

function ResetBtn({ show, onClick, disabled, title = "Reset to template" }: { show: boolean; onClick: () => void; disabled?: boolean; title?: string }) {
  if (!show) return null;
  return (
    <button className="btn s sq ghost reset" title={title} disabled={disabled} onClick={onClick}>
      <i className="ic ic-x" />
    </button>
  );
}

function SectionPanel({ doc, errors, editable, sel, setSel, edit, onIssue, index }: Props & { index: number }) {
  const s = doc.sections[index];
  const r = s.resolved ?? resolveSection(doc, index);
  const spans = sectionSpans(doc);
  const span = spans[index];
  const bars = r.bars.value;
  const perPass = bars * secondsPerBar(doc);
  const next = doc.sections[index + 1]?.id;
  const layers = deriveLayers(doc)[index] ?? [];
  const [renaming, setRenaming] = useState(false);
  useEffect(() => setRenaming(false), [index]);
  const cells = sel.kind === "cells" ? sel.cells : [];
  const cellTracks = new Set(cells.filter((c) => c.section === index).map((c) => c.track));
  const templates = doc.sections.filter((_, i) => i !== index).map((x) => x.id);
  const hasRepeat = s.repeat !== undefined;

  const fieldReset = (field: "bars" | "autorelease" | "quantize") =>
    hasRepeat && s[field] !== undefined ? () => edit((d) => ops.patchSection(d, index, { [field]: null })) : null;

  const nextWords = next ? <b>{next}</b> : "the end of the score";
  const flow = r.autorelease.value ? (
    <>
      Plays {plural(bars, "bar")} once, then moves on to {nextWords} by itself.
    </>
  ) : (
    <>
      The section loops until you press <b>Release</b>. The change to {nextWords} then happens{" "}
      {r.quantize.value === "bar" ? "at the next bar line" : `at the end of the running ${bars}-bar pass`}.
    </>
  );

  const pick = (track: number, state: ScoreState) => {
    const targets = cellTracks.size > 1 && cellTracks.has(track) ? cells : [{ section: index, track }];
    edit((d) => ops.setCellState(d, targets, state));
  };

  const single = cells.length === 1 ? cells[0] : null;
  const multi = cells.length > 1;

  return (
    <>
      <div className="insp-h">
        <div className="kicker">
          <span className="caps">
            Section {index + 1} of {doc.sections.length}
          </span>
          {multi && <span className="pill sel-pill">{plural(cells.length, "clip")} selected</span>}
        </div>
        <h2>
          {renaming ? (
            <Field
              value={s.id}
              mono
              big
              check={(v) => ops.nameProblem(v, doc.sections.map((x) => x.id).filter((x) => x !== s.id), "section")}
              onCommit={(v) => {
                setRenaming(false);
                edit((d) => ops.renameSection(d, index, v.trim()));
              }}
            />
          ) : (
            <span className="mono" onDoubleClick={() => editable && setRenaming(true)} title={editable ? "Double-click to rename" : undefined}>
              {s.id}
            </span>
          )}
          {!renaming && editable && (
            <button className="btn s sq ghost" title="Rename (references in repeat move along)" onClick={() => setRenaming(true)}>
              <i className="ic ic-code" />
            </button>
          )}
        </h2>
        <div className="sub">
          {span.bars === 1 ? `Bar ${span.start + 1}` : `Bars ${span.start + 1}–${span.start + span.bars}`} · {duration(perPass)} per pass at {doc.bpm} BPM
        </div>
      </div>
      <Problems
        errors={errors}
        onIssue={onIssue}
        filter={(e) => (e.at?.kind === "section" && e.at.index === index) || (e.at?.kind === "section_track" && e.at.section === index)}
      />
      <div className="insp-s">
        <span className="caps">Flow</span>
        <div className="frow">
          <label>Template</label>
          <div>
            <span className="field sel" style={{ flex: 1 }}>
              <i className="ic ic-link" style={{ "--ic": "12px", color: hasRepeat ? "var(--accent)" : "var(--tx-4)" } as React.CSSProperties} />
              <select
                value={s.repeat ?? ""}
                disabled={!editable}
                onChange={(e) => edit((d) => ops.patchSection(d, index, { repeat: e.target.value === "" ? null : e.target.value }))}
              >
                <option value="">none</option>
                {templates.map((id) => (
                  <option key={id} value={id}>
                    {id}
                  </option>
                ))}
                {s.repeat !== undefined && !templates.includes(s.repeat) && <option value={s.repeat}>{s.repeat} (missing)</option>}
              </select>
              <i className="ic ic-chev-d chev" />
            </span>
          </div>
        </div>
        <div className="frow">
          <label>Bars</label>
          <div>
            <Field
              value={String(bars)}
              width={64}
              unit={bars === 1 ? "bar" : "bars"}
              disabled={!editable}
              check={(v) => (/^\d+$/.test(v.trim()) && Number(v) >= 1 && Number(v) <= 256 ? null : "1 to 256")}
              onCommit={(v) => edit((d) => ops.patchSection(d, index, { bars: Number(v) }))}
            />
            <InhTag from={r.bars.from} />
            <ResetBtn show={!!fieldReset("bars")} disabled={!editable} onClick={() => fieldReset("bars")?.()} />
          </div>
        </div>
        <div className="frow stack">
          <label>
            Change <InhTag from={r.autorelease.from} />
            <ResetBtn show={!!fieldReset("autorelease")} disabled={!editable} onClick={() => fieldReset("autorelease")?.()} />
          </label>
          <div>
            <Seg
              value={r.autorelease.value}
              disabled={!editable}
              onPick={(v) => edit((d) => ops.patchSection(d, index, { autorelease: v }))}
              options={[
                { value: true, icon: "ic-auto", label: "Auto" },
                { value: false, icon: "ic-hand", label: "On release" },
              ]}
            />
          </div>
        </div>
        <div className="frow stack">
          <label>
            Release quantize <InhTag from={r.quantize.from} />
            <ResetBtn show={!!fieldReset("quantize")} disabled={!editable} onClick={() => fieldReset("quantize")?.()} />
          </label>
          <div>
            <Seg
              value={r.quantize.value}
              disabled={!editable}
              onPick={(v) => edit((d) => ops.patchSection(d, index, { quantize: v }))}
              options={[
                { value: "bar", icon: "ic-bar", label: "Next bar" },
                { value: "loop", icon: "ic-loop", label: "End of loop" },
              ]}
            />
          </div>
        </div>
        <div className="hint">
          {flow}
          {r.autorelease.value && " Quantize only matters for sections that wait for Release."}
        </div>
      </div>

      <div className="insp-s" style={{ gap: 10 }}>
        <span className="caps">
          State per track{" "}
          <span className="dim2" style={{ letterSpacing: 0, textTransform: "none", fontWeight: 500 }}>
            · <i className="ic ic-link" style={{ "--ic": "9px" } as React.CSSProperties} /> inherited, ● overridden
          </span>
        </span>
        {doc.tracks.map((t, ti) => {
          const st = r.tracks[ti];
          const own = st?.from.kind === "own";
          const overridden = hasRepeat && own;
          return (
            <div key={ti} className={`stpick${cellTracks.has(ti) ? " sel" : ""}`}>
              <button className="tn" onClick={() => setSel({ kind: "cells", cells: [{ section: index, track: ti }], anchor: { section: index, track: ti }, focus: { section: index, track: ti } })}>
                <i className="c" style={{ "--c": `var(--trk-${(ti % 8) + 1})` } as React.CSSProperties} />
                {t.name}
                {st?.from.kind === "repeat" && <i className="ic ic-link" style={{ "--ic": "9px", color: "var(--accent)" } as React.CSSProperties} />}
              </button>
              <div className="opts">
                {STATES.map((state) => {
                  const look = STATE_LOOK[state];
                  return (
                    <button
                      key={state}
                      className={`${look.cls}${st?.state === state ? " on" : ""}`}
                      disabled={!editable}
                      title={`${look.word} (${look.key})`}
                      onClick={() => pick(ti, state)}
                    >
                      <i className={`ic ${look.icon}`} />
                    </button>
                  );
                })}
                {overridden && (
                  <button
                    className="ovr-dot"
                    title="Overrides the template — click to reset"
                    disabled={!editable}
                    onClick={() => edit((d) => ops.resetCells(d, [{ section: index, track: ti }]))}
                  />
                )}
              </div>
            </div>
          );
        })}
        <div className="legend">
          {STATES.map((state) => (
            <span key={state}>
              <i className={`ic ${STATE_LOOK[state].icon}`} style={{ color: `var(--st-${STATE_LOOK[state].cls})` }} />
              {STATE_LOOK[state].word}
            </span>
          ))}
        </div>
      </div>

      {single && single.section === index && doc.tracks[single.track] && (
        <ClipNote doc={doc} index={index} track={single.track} layers={layers[single.track]} editable={editable} edit={edit} />
      )}
      {multi && (
        <div className="insp-s">
          <span className="caps">{plural(cells.length, "clip")}</span>
          <div className="opts multi">
            {STATES.map((state) => (
              <button key={state} className={STATE_LOOK[state].cls} disabled={!editable} onClick={() => edit((d) => ops.setCellState(d, cells, state))}>
                <i className={`ic ${STATE_LOOK[state].icon}`} />
                {STATE_LOOK[state].word}
              </button>
            ))}
          </div>
          <button className="btn s" disabled={!editable} onClick={() => edit((d) => ops.resetCells(d, cells))}>
            Reset all to template
          </button>
        </div>
      )}

      <div className="insp-f">
        <button className="btn s" disabled={!editable} onClick={() => { edit((d) => ops.duplicateSection(d, index)); setSel({ kind: "section", index: index + 1 }); }}>
          <i className="ic ic-dup" />
          Duplicate
        </button>
        <button className="btn s sq" title="Move left" disabled={!editable || index === 0} onClick={() => { edit((d) => ops.moveSection(d, index, index - 1)); setSel({ kind: "section", index: index - 1 }); }}>
          <i className="ic ic-arr-l" />
        </button>
        <button className="btn s sq" title="Move right" disabled={!editable || index >= doc.sections.length - 1} onClick={() => { edit((d) => ops.moveSection(d, index, index + 1)); setSel({ kind: "section", index: index + 1 }); }}>
          <i className="ic ic-arr-r" />
        </button>
        <div className="grow" />
        <button className="btn s danger" disabled={!editable || doc.sections.length <= 1} onClick={() => { edit((d) => ops.deleteSection(d, index)); setSel({ kind: "score" }); }}>
          <i className="ic ic-trash" />
          Delete
        </button>
      </div>
    </>
  );
}

function ClipNote({ doc, index, track, layers, editable, edit }: { doc: ScoreDoc; index: number; track: number; layers?: { layers: number; writes: number | null; stacked: boolean }; editable: boolean; edit: Props["edit"] }) {
  const s = doc.sections[index];
  const r = s.resolved ?? resolveSection(doc, index);
  const t = doc.tracks[track];
  const st = r.tracks[track];
  const look = STATE_LOOK[st.state];
  const bars = r.bars.value;
  const name = <b>{t.name}</b>;
  let what: ReactNode;
  switch (st.state) {
    case "record":
      what = <>{name} records a new {bars}-bar loop. Whatever this track held before is replaced.{layers?.stacked && " Later sections stack more layers on it."}</>;
      break;
    case "overdub":
      what = layers && layers.layers > 1 ? <>{name} keeps playing and records layer {layers.writes} on top.</> : <>{name} has no loop yet at this point, so this overdub records the first one.</>;
      break;
    case "play":
      what = layers && layers.layers > 0 ? <>{name} plays its loop{layers.layers > 1 ? ` (${layers.layers} layers)` : ""}.</> : <>Nothing has been recorded on {name} yet at this point, so it stays silent.</>;
      break;
    case "stop":
      what = <>{name} is silent. Its loop is kept for later sections.</>;
      break;
    case "hear_through":
      what = t.instrument ? (
        <>{name} is an instrument track: it is heard whenever it is played, so Monitor changes nothing here.</>
      ) : (
        <>You hear {name}’s input live; nothing is recorded.</>
      );
      break;
  }
  const needsMonitor = !t.instrument && (st.state === "record" || st.state === "overdub" || st.state === "hear_through");
  const own = st.from.kind === "own";
  const ownGrid = s.tracks.find((x) => x.track === t.name)?.grid;
  return (
    <div className="insp-s">
      <span className="caps">
        Clip · {t.name} × {s.id}
      </span>
      <div className="clipnote">
        <span className={`st ${look.cls}`}>
          <i className={`ic ${look.icon}`} />
          {look.word}
        </span>
        <span className="dim">{st.from.kind === "repeat" ? `inherited from ${st.from.section}` : st.from.kind === "default" ? "not stated — stopped" : s.repeat ? "overrides the template" : "stated in this section"}</span>
      </div>
      <div className="hint">
        {what}
        {needsMonitor && t.monitor === false && <span className="warnline"> Monitoring is off for {t.name}, so you will not hear the input.</span>}
      </div>
      {t.loop === "midi" && (
        <div className="frow">
          <label>{T.noteGrid}</label>
          <div>
            <select
              className="field sel"
              value={ownGrid ?? ""}
              disabled={!editable}
              title="This clip's note grid: while the section runs, the track's MIDI layers play on it"
              onChange={(e) => edit((d) => ops.setCellGrid(d, { section: index, track }, e.target.value === "" ? undefined : e.target.value))}
            >
              <option value="">Track ({GRID_CHOICES.find((g) => g.value === (t.grid ?? "none"))?.label})</option>
              {GRID_CHOICES.map((g) => (
                <option key={g.value} value={g.value}>
                  {g.label}
                </option>
              ))}
            </select>
          </div>
        </div>
      )}
      {own && (
        <button className="btn s" disabled={!editable} onClick={() => edit((d) => ops.resetCells(d, [{ section: index, track }]))}>
          {s.repeat ? `Reset to template (${s.repeat})` : "Clear (back to Stop)"}
        </button>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// A track
// ---------------------------------------------------------------------------------------------

function TrackPanel({ doc, errors, notes, editable, setSel, edit, onIssue, index }: Props & { index: number }) {
  const t = doc.tracks[index];
  const monitor = t.monitor !== false;
  const loopMain = busHas(t.bus, "main+monitor", "main");
  const loopMon = busHas(t.bus, "main+monitor", "monitor");
  const liveMain = busHas(t.monitor_bus, "monitor", "main");
  const liveMon = busHas(t.monitor_bus, "monitor", "monitor");
  const pan = t.pan ?? 0;
  const patch = (patch: ops.TrackPatch) => edit((d) => ops.patchTrack(d, index, patch));
  const used = doc.sections.filter((s) => s.resolved?.tracks[index] && s.resolved.tracks[index].state !== "stop").length;
  const inputCheck = (v: string) => (/^\d+$/.test(v.trim()) && Number(v) >= 1 && Number(v) <= 64 ? null : "Input 1 to 64");
  return (
    <>
      <div className="insp-h">
        <div className="kicker">
          <span className="caps">
            Track {index + 1} of {doc.tracks.length}
          </span>
        </div>
        <h2>
          <i className="trk" style={{ "--c": `var(--trk-${(index % 8) + 1})` } as React.CSSProperties} />
          <Field
            value={t.name}
            mono
            big
            disabled={!editable}
            check={(v) => ops.nameProblem(v, doc.tracks.map((x) => x.name).filter((x) => x !== t.name), "track")}
            onCommit={(v) => edit((d) => ops.renameTrack(d, index, v.trim()))}
          />
        </h2>
        <div className="sub">
          {t.instrument ? `${T.instrument} · stereo` : `${t.input_right ? "stereo" : "mono"} · ${inputLabel(t)}`} · active in {plural(used, "section")}
        </div>
      </div>
      <Problems errors={errors} onIssue={onIssue} filter={(e) => e.at?.kind === "track" && e.at.index === index} />
      {(notes ?? [])
        .filter((e) => e.at?.kind === "track" && e.at.index === index)
        .map((e, i) => (
          <div key={i} className="insp-s">
            <div className="diag">
              <i className="ic ic-info" />
              <div>{e.message}</div>
            </div>
          </div>
        ))}
      <div className="insp-s">
        <span className="caps">Input</span>
        <div className="frow">
          <label>Source</label>
          <div>
            <select
              className="field sel"
              value={t.instrument ? "instrument" : "audio"}
              disabled={!editable}
              title="An input of the interface, or an instrument plugin played over MIDI"
              onChange={(e) => edit((d) => ops.setTrackInstrument(d, index, e.target.value === "instrument"))}
            >
              <option value="audio">Audio input</option>
              <option value="instrument">{T.instrument} (MIDI)</option>
            </select>
          </div>
        </div>
        {t.instrument ? (
          <>
            <InstrumentRows doc={doc} track={t} editable={editable} edit={edit} patch={patch} />
            <div className="frow">
              <label>{T.loopKind}</label>
              <div>
                <select
                  className="field sel"
                  value={t.loop === "midi" ? "midi" : "audio"}
                  disabled={!editable}
                  title="What a take stores: the instrument's sound (Audio) or the notes, played back through the instrument (MIDI)"
                  onChange={(e) => patch({ loop: e.target.value === "midi" ? "midi" : undefined, ...(e.target.value === "midi" ? {} : { grid: undefined }) })}
                >
                  <option value="audio">{T.loopAudio}</option>
                  <option value="midi">{T.loopMidi}</option>
                </select>
              </div>
            </div>
            {t.loop === "midi" && (
              <div className="frow">
                <label>{T.noteGrid}</label>
                <div>
                  <select
                    className="field sel"
                    value={t.grid ?? "none"}
                    disabled={!editable}
                    title="Notes play on this grid - the recorded ones too. The recording itself never changes."
                    onChange={(e) => patch({ grid: e.target.value === "none" ? undefined : e.target.value })}
                  >
                    {GRID_CHOICES.map((g) => (
                      <option key={g.value} value={g.value}>
                        {g.label}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
            )}
          </>
        ) : (
          <>
            <div className="frow">
              <label>Input</label>
              <div>
                <Field value={String(t.input)} width={64} disabled={!editable} check={inputCheck} onCommit={(v) => patch({ input: Number(v) })} />
                <Toggle
                  on={t.input_right !== undefined}
                  disabled={!editable}
                  label="Stereo"
                  onChange={(on) => patch({ input_right: on ? t.input + 1 : undefined })}
                />
              </div>
            </div>
            {t.input_right !== undefined && (
              <div className="frow">
                <label>Right input</label>
                <div>
                  <Field value={String(t.input_right)} width={64} disabled={!editable} check={inputCheck} onCommit={(v) => patch({ input_right: Number(v) })} />
                </div>
              </div>
            )}
          </>
        )}
        <div className="frow">
          <label>{t.instrument ? "Heard" : "Monitoring"}</label>
          <div>
            <Toggle
              on={monitor}
              disabled={!editable}
              label={t.instrument ? (monitor ? "Always" : "Never") : monitor ? "Allowed" : "Off"}
              onChange={(on) => patch({ monitor: on ? undefined : false })}
            />
          </div>
        </div>
        <div className="hint">
          {t.instrument
            ? monitor
              ? "An instrument track is heard whenever it is played — in every section, whatever its clips say."
              : "This instrument is never heard live; its takes are still recorded and play back."
            : monitor
              ? "You can hear this input live through the engine — Record, Overdub and Monitor need that."
              : "This input is never heard live. Record, Overdub and Monitor still work, but without hearing yourself."}
        </div>
      </div>
      <div className="insp-s">
        <span className="caps">Output</span>
        <div className="frow">
          <label>Pan</label>
          <div>
            <input
              type="range"
              className="range"
              min={-1}
              max={1}
              step={0.05}
              value={pan}
              disabled={!editable}
              onChange={() => undefined}
              onPointerUp={(e) => {
                const v = Number((e.target as HTMLInputElement).value);
                if (v !== pan) patch({ pan: v === 0 ? undefined : v });
              }}
              onKeyUp={(e) => {
                const v = Number((e.target as HTMLInputElement).value);
                if (v !== pan) patch({ pan: v === 0 ? undefined : v });
              }}
              onInput={(e) => {
                const out = (e.target as HTMLInputElement).nextElementSibling;
                if (out) out.textContent = panWord(Number((e.target as HTMLInputElement).value));
              }}
            />
            <span className="panv">{panWord(pan)}</span>
          </div>
        </div>
        <div className="frow">
          <label>Loop to</label>
          <div>
            <BusPair main={loopMain} mon={loopMon} disabled={!editable} onChange={(m, n) => patch({ bus: m && n ? undefined : busWord(m, n) })} />
          </div>
        </div>
        <div className="frow">
          <label>Live input to</label>
          <div>
            <BusPair main={liveMain} mon={liveMon} disabled={!editable} onChange={(m, n) => patch({ monitor_bus: !m && n ? undefined : busWord(m, n) })} />
          </div>
        </div>
        <div className="hint">Main is the room (PA), Monitor the headphones. The click is only ever on Monitor.</div>
      </div>
      <div className="insp-s">
        <span className="caps">Latency</span>
        {!t.instrument && (
        <div className="frow">
          <label>Measured</label>
          <div>
            <Field
              value={t.latency === undefined ? "" : String(t.latency)}
              width={90}
              unit="frames"
              placeholder="auto"
              disabled={!editable}
              check={(v) => (v.trim() === "" || (/^\d+$/.test(v.trim()) && Number(v) <= 96000) ? null : "0 to 96000, empty = engine default")}
              onCommit={(v) => patch({ latency: v.trim() === "" ? undefined : Number(v) })}
            />
          </div>
        </div>
        )}
        <div className="frow">
          <label>Trim</label>
          <div>
            <Field
              value={String(t.latency_trim ?? 0)}
              width={90}
              unit="frames"
              disabled={!editable}
              check={(v) => (/^-?\d+$/.test(v.trim()) && Math.abs(Number(v)) <= 96000 ? null : "−96000 to 96000")}
              onCommit={(v) => patch({ latency_trim: Number(v) === 0 ? undefined : Number(v) })}
            />
          </div>
        </div>
        <div className="hint">
          {t.instrument
            ? "Automatic for an instrument: output latency + one block + the instrument’s own latency. Trim shifts the takes by ear."
            : "What a recording on this input is shifted back by. Empty uses the engine’s measured default; trim adds to it."}
        </div>
      </div>
      <div className="insp-f">
        <button className="btn s sq" title="Move up" disabled={!editable || index === 0} onClick={() => { edit((d) => ops.moveTrack(d, index, index - 1)); setSel({ kind: "track", index: index - 1 }); }}>
          <i className="ic ic-arr-l" style={{ transform: "rotate(90deg)" }} />
        </button>
        <button className="btn s sq" title="Move down" disabled={!editable || index >= doc.tracks.length - 1} onClick={() => { edit((d) => ops.moveTrack(d, index, index + 1)); setSel({ kind: "track", index: index + 1 }); }}>
          <i className="ic ic-arr-r" style={{ transform: "rotate(90deg)" }} />
        </button>
        <div className="grow" />
        <button className="btn s danger" disabled={!editable || doc.tracks.length <= 1} onClick={() => { edit((d) => ops.deleteTrack(d, index)); setSel({ kind: "score" }); }}>
          <i className="ic ic-trash" />
          Delete track
        </button>
      </div>
    </>
  );
}

function panWord(v: number): string {
  if (Math.abs(v) < 0.001) return "C";
  return v < 0 ? `L ${Math.round(-v * 100)}` : `R ${Math.round(v * 100)}`;
}

/** `36-51` as two note numbers, or null when it is not a range of MIDI notes. */
function noteRange(text: string): [number, number] | null {
  const m = /^\s*(\d{1,3})\s*-\s*(\d{1,3})\s*$/.exec(text);
  if (!m) return null;
  const [low, high] = [Number(m[1]), Number(m[2])];
  return low <= high && high <= 127 ? [low, high] : null;
}

/** The MIDI side of an instrument track: plugin (in `mixer:`), device, channel, keys. */
function InstrumentRows({
  doc,
  track,
  editable,
  edit,
  patch,
}: {
  doc: ScoreDoc;
  track: DocTrack;
  editable: boolean;
  edit: Props["edit"];
  patch: (p: ops.TrackPatch) => void;
}) {
  const { catalog } = usePlugins();
  const [ports, setPorts] = useState<string[]>([]);
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  useEffect(() => {
    midiApi.ports().then((list) => setPorts(list.map((p) => p.name)), () => setPorts([]));
  }, []);
  const midi = track.midi ?? {};
  const setMidi = (next: TrackMidi) => {
    const clean = Object.fromEntries(Object.entries(next).filter(([, v]) => v !== undefined && v !== null)) as TrackMidi;
    patch({ midi: Object.keys(clean).length > 0 ? clean : undefined });
  };
  const id = doc.mixer?.tracks.find((m) => m.track === track.name)?.instrument;
  const plugin = id ? catalog?.plugins.find((p) => p.id === id) : undefined;
  const pluginLabel = id ? (plugin ? plugin.name : catalog ? `${id} (not installed)` : id) : "Keep the local one";
  const devices = [
    { value: "", label: T.allDevices },
    ...ports.map((p) => ({ value: p, label: p })),
    ...(midi.device && !ports.includes(midi.device) ? [{ value: midi.device, label: `${midi.device} (not connected)` }] : []),
  ];
  const range = midi.note_low !== undefined && midi.note_high !== undefined ? `${midi.note_low}-${midi.note_high}` : "";
  return (
    <>
      <div className="frow">
        <label>{T.instrument}</label>
        <div>
          <button
            className="btn s"
            style={{ maxWidth: 170, overflow: "hidden", justifyContent: "flex-start" }}
            disabled={!editable}
            title={id ? `${id} - click to choose another` : "No instrument named in the score: the track plays what it has on this computer. Click to choose one."}
            onClick={(e) => setAnchor(anchor ? null : e.currentTarget)}
          >
            <i className="ic ic-keys" />
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{pluginLabel}</span>
          </button>
          {id && (
            <button className="btn s sq ghost" disabled={!editable} title="Name no instrument in the score" onClick={() => edit((d) => ops.setTrackInstrumentPlugin(d, track.name, undefined))}>
              <i className="ic ic-x" />
            </button>
          )}
          {anchor && (
            <InstrumentPicker
              track={track.name}
              anchor={anchor}
              onClose={() => setAnchor(null)}
              onPick={(pick) => edit((d) => ops.setTrackInstrumentPlugin(d, track.name, pick))}
            />
          )}
        </div>
      </div>
      <div className="frow">
        <label>Device</label>
        <div>
          <select className="field sel" value={midi.device ?? ""} disabled={!editable} title="Which MIDI keyboard plays this track" onChange={(e) => setMidi({ ...midi, device: e.target.value || undefined })}>
            {devices.map((d) => (
              <option key={d.value} value={d.value}>
                {d.label}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="frow">
        <label>Channel</label>
        <div>
          <select
            className="field sel"
            value={midi.channel ?? 0}
            disabled={!editable}
            title="Which MIDI channel plays this track. Notes on other channels stay with the MIDI bindings."
            onChange={(e) => setMidi({ ...midi, channel: Number(e.target.value) || undefined })}
          >
            <option value={0}>{T.allChannels}</option>
            {Array.from({ length: 16 }, (_, c) => (
              <option key={c + 1} value={c + 1}>
                {T.channelShort} {c + 1}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="frow">
        <label>Keys</label>
        <div>
          <Field
            value={range}
            width={90}
            mono
            placeholder="all"
            disabled={!editable}
            check={(v) => (v.trim() === "" || noteRange(v) ? null : "Lowest-highest, MIDI notes 0 to 127, e.g. 36-51")}
            onCommit={(v) => {
              const r = noteRange(v);
              setMidi({ ...midi, note_low: r?.[0], note_high: r?.[1] });
            }}
          />
          {range && (
            <span className="dim">
              {noteName(midi.note_low as number)}–{noteName(midi.note_high as number)}
            </span>
          )}
        </div>
      </div>
      <div className="frow">
        <label>{T.transpose}</label>
        <div>
          <TransposeControl
            value={midi.transpose ?? 0}
            disabled={!editable}
            onChange={(v) => setMidi({ ...midi, transpose: v === 0 ? undefined : v })}
          />
        </div>
      </div>
      <div className="hint">
        Only the plugin’s id goes into the score; its sound stays on this computer. Keys outside the range stay with the MIDI bindings; the range is checked before the transposition.
      </div>
    </>
  );
}

function BusPair({ main, mon, onChange, disabled }: { main: boolean; mon: boolean; onChange: (main: boolean, mon: boolean) => void; disabled?: boolean }) {
  return (
    <div className="buspair">
      <button className={`chip bus${main ? " on" : ""}`} disabled={disabled} onClick={() => onChange(!main, mon)}>
        <i className="ic ic-speaker" />
        Main
      </button>
      <button className={`chip bus${mon ? " on" : ""}`} disabled={disabled} onClick={() => onChange(main, !mon)}>
        <i className="ic ic-phones" />
        Monitor
      </button>
    </div>
  );
}
