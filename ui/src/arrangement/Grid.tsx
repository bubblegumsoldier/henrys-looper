//! The grid: sections across, tracks down, one state clip per cell.
//!
//! ```text
//!  corner │ ruler: section markers (drag = move, right edge = length) + bar numbers + playhead
//!  ───────┼──────────────────────────────────────────────────────────────────────────────────
//!  heads  │ lanes: one clip per section, width proportional to its bars
//! ```
//!
//! One scroll container holds all four quarters; the ruler and the heads are `sticky`, so they
//! scroll along one axis each, the way a DAW's do. Widths are CSS: `--b` is pixels per bar (the
//! zoom), a clip's `--s`/`--n` are its start and length in bars - the same variables the mockup
//! (design/arrangement.html) uses, so its stylesheet applies unchanged.

import { useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { gotoAddress } from "../midi/addresses";
import { useStatusEffect } from "../status";
import type { DocTrack, ModelIssue, ScoreDoc } from "../types";
import { STATE_LOOK, plural } from "./labels";
import { MAX_TRACKS, nameProblem } from "./ops";
import * as ops from "./ops";
import { beatsPerBar, deriveLayers, resolveSection, type SectionSpan } from "./resolve";
import { cellKey, clickCell, selectedSections, type Selection } from "./selection";
import type { Run } from "./useDoc";

/** Left offset of bar 1 inside the lanes, as in the mockup. */
const X0 = 14;

interface Props {
  doc: ScoreDoc;
  errors: ModelIssue[];
  editable: boolean;
  run: Run;
  sel: Selection;
  setSel: (s: Selection) => void;
  /** Pixels per bar. */
  zoom: number;
  edit: (op: (d: ScoreDoc) => ScoreDoc) => void;
  onGoto: (section: number) => void;
  engineTracks: Record<string, { state: string; layers: number; channels: number }>;
}

/** Engine track state -> chip look and word. */
const RUNTIME: Record<string, { cls: string; icon: string; word: string }> = {
  empty: { cls: "empty", icon: "ic-empty", word: "empty" },
  armed: { cls: "armed", icon: "ic-armed", word: "armed" },
  recording: { cls: "rec", icon: "ic-rec", word: "recording" },
  overdub: { cls: "dub", icon: "ic-dub", word: "overdub" },
  ready: { cls: "ready", icon: "ic-play-o", word: "ready" },
  playing: { cls: "play", icon: "ic-play", word: "playing" },
};

export function inputLabel(t: DocTrack): string {
  return t.input_right ? `In ${t.input}–${t.input_right}` : `In ${t.input}`;
}

/** Which elements carry an error, for the red marks. */
export function errorIndex(errors: ModelIssue[]) {
  const sections = new Map<number, string>();
  const tracks = new Map<number, string>();
  const cells = new Map<string, string>();
  for (const e of errors) {
    const at = e.at;
    if (!at) continue;
    if (at.kind === "section") sections.set(at.index, e.message);
    else if (at.kind === "track") tracks.set(at.index, e.message);
    else if (at.kind === "section_track") cells.set(`${at.section}:${at.track}`, e.message);
  }
  return { sections, tracks, cells };
}

type Drag =
  | { kind: "move"; from: number; startX: number; to: number; active: boolean }
  | { kind: "bars"; index: number; startX: number; startBars: number; bars: number }
  | { kind: "track"; from: number; startY: number; to: number; active: boolean };

export function Grid({ doc, errors, editable, run, sel, setSel, zoom, edit, onGoto, engineTracks }: Props) {
  const [drag, setDrag] = useState<Drag | null>(null);
  const [renaming, setRenaming] = useState<{ kind: "section" | "track"; index: number } | null>(null);
  const rulerRef = useRef<HTMLDivElement>(null);
  const headsRef = useRef<HTMLDivElement>(null);

  // Spans with a bar-drag applied, so the edge follows the pointer before anything is rendered.
  const spans: SectionSpan[] = useMemo(() => {
    let at = 0;
    return doc.sections.map((s, i) => {
      let bars = Math.max(1, (s.resolved ?? resolveSection(doc, i)).bars.value);
      if (drag?.kind === "bars" && drag.index === i) bars = drag.bars;
      const span = { start: at, bars };
      at += bars;
      return span;
    });
  }, [doc, drag]);
  const total = spans.reduce((n, s) => n + s.bars, 0);
  const layers = useMemo(() => deriveLayers(doc), [doc]);
  const errs = useMemo(() => errorIndex(errors), [errors]);
  const bpb = beatsPerBar(doc);
  const picked = selectedSections(sel);
  const cellSel = new Set(sel.kind === "cells" ? sel.cells.map(cellKey) : []);
  const rowh = 96;

  // --- the runner's position, in bars from the start ------------------------------------------
  const pos = (() => {
    if (run.section === null || !spans[run.section]) return 0;
    const span = spans[run.section];
    return span.start + Math.max(0, run.bar - 1) + Math.max(0, run.beat - 1) / bpb;
  })();
  const sounding = run.playing && run.section !== null;
  const armPos = run.armed && sounding ? pos + run.armedBars + run.armedBeats / bpb : null;

  // --- pointer handling --------------------------------------------------------------------------
  const barAt = (clientX: number) => {
    const r = rulerRef.current?.getBoundingClientRect();
    return r ? (clientX - r.left - X0) / zoom : 0;
  };
  const sectionAtBar = (bar: number) => {
    const i = spans.findIndex((s) => bar < s.start + s.bars);
    return i < 0 ? spans.length - 1 : Math.max(0, i);
  };

  const onMarkerDown = (e: ReactPointerEvent, index: number) => {
    if (e.button !== 0 || renaming) return;
    setDrag({ kind: "move", from: index, startX: e.clientX, to: index, active: false });
  };
  const onEdgeDown = (e: ReactPointerEvent, index: number) => {
    if (e.button !== 0 || !editable) return;
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setDrag({ kind: "bars", index, startX: e.clientX, startBars: spans[index].bars, bars: spans[index].bars });
  };
  const onRulerMove = (e: ReactPointerEvent) => {
    if (!drag) return;
    if (drag.kind === "move") {
      const active = drag.active || (editable && Math.abs(e.clientX - drag.startX) > 5);
      if (active && !drag.active) rulerRef.current?.setPointerCapture(e.pointerId);
      setDrag({ ...drag, active, to: active ? sectionAtBar(barAt(e.clientX)) : drag.from });
    } else if (drag.kind === "bars") {
      const bars = Math.max(1, Math.min(256, Math.round(drag.startBars + (e.clientX - drag.startX) / zoom)));
      if (bars !== drag.bars) setDrag({ ...drag, bars });
    }
  };
  const onRulerUp = (e: ReactPointerEvent) => {
    if (!drag) return;
    setDrag(null);
    if (drag.kind === "move") {
      if (drag.active) {
        if (drag.to !== drag.from) {
          edit((d) => ops.moveSection(d, drag.from, drag.to));
          setSel({ kind: "section", index: drag.to });
        }
      } else if (run.playing) {
        onGoto(drag.from);
      } else if (e.detail < 2) {
        setSel({ kind: "section", index: drag.from });
      }
    } else if (drag.kind === "bars" && drag.bars !== drag.startBars) {
      edit((d) => ops.patchSection(d, drag.index, { bars: drag.bars }));
    }
  };

  const onHeadDown = (e: ReactPointerEvent, index: number) => {
    if (e.button !== 0 || renaming || (e.target as HTMLElement).closest(".chip, .knob, input")) return;
    setDrag({ kind: "track", from: index, startY: e.clientY, to: index, active: false });
  };
  const onHeadMove = (e: ReactPointerEvent) => {
    if (drag?.kind !== "track") return;
    const active = drag.active || (editable && Math.abs(e.clientY - drag.startY) > 5);
    if (active && !drag.active) headsRef.current?.setPointerCapture(e.pointerId);
    const r = headsRef.current?.getBoundingClientRect();
    const to = r ? Math.max(0, Math.min(doc.tracks.length - 1, Math.floor((e.clientY - r.top) / rowh))) : drag.from;
    setDrag({ ...drag, active, to: active ? to : drag.from });
  };
  const onHeadUp = () => {
    if (drag?.kind !== "track") return;
    setDrag(null);
    if (drag.active && drag.to !== drag.from) {
      edit((d) => ops.moveTrack(d, drag.from, drag.to));
      setSel({ kind: "track", index: drag.to });
    } else if (!drag.active) {
      setSel({ kind: "track", index: drag.from });
    }
  };

  const commitRename = (value: string) => {
    if (!renaming) return;
    const { kind, index } = renaming;
    const current = kind === "section" ? doc.sections[index]?.id : doc.tracks[index]?.name;
    setRenaming(null);
    if (value === current) return;
    const taken = kind === "section" ? doc.sections.map((s) => s.id) : doc.tracks.map((t) => t.name);
    if (nameProblem(value, taken.filter((n) => n !== current), kind) !== null) return;
    edit((d) => (kind === "section" ? ops.renameSection(d, index, value.trim()) : ops.renameTrack(d, index, value.trim())));
  };

  // --- bar numbers -------------------------------------------------------------------------------
  const step = zoom >= 18 ? 2 : zoom >= 9 ? 4 : 8;
  const starts = new Set(spans.map((s) => s.start));
  const numbers: { bar: number; strong: boolean }[] = [];
  for (let b = 0; b < total; b++) {
    if (starts.has(b)) numbers.push({ bar: b, strong: true });
    else if (b % step === 0 && ![...starts].some((s) => Math.abs(s - b) * zoom < 14)) numbers.push({ bar: b, strong: false });
  }

  const width = X0 + total * zoom + 90;
  const moveTarget = drag?.kind === "move" && drag.active ? drag : null;
  const trackTarget = drag?.kind === "track" && drag.active ? drag : null;

  return (
    <div
      className={`arr${editable ? "" : " readonly"}${drag ? " dragging" : ""}`}
      style={{ "--b": `${zoom}px`, "--x0": `${X0}px`, "--rowh": `${rowh}px`, "--bpb": bpb, "--w": `${width}px` } as React.CSSProperties}
    >
      {/* corner */}
      <div className="corner">
        <div className="row">
          <span className="caps">Tracks</span>
          <span className="dim2" style={{ fontSize: 10 }}>
            {doc.tracks.length} / {MAX_TRACKS}
          </span>
        </div>
        <div className="row" style={{ justifyContent: "space-between" }}>
          <span className="dim" style={{ fontSize: 11 }}>
            {plural(doc.sections.length, "section")} · {plural(total, "bar")}
          </span>
          <button
            className="btn s sq ghost"
            title="Add track"
            disabled={!editable || doc.tracks.length >= MAX_TRACKS}
            onClick={() => {
              edit((d) => ops.addTrack(d));
              setSel({ kind: "track", index: doc.tracks.length });
            }}
          >
            <i className="ic ic-plus" />
          </button>
        </div>
      </div>

      {/* ruler */}
      <div className="ruler" ref={rulerRef} onPointerMove={onRulerMove} onPointerUp={onRulerUp} onPointerCancel={() => setDrag(null)}>
        {doc.sections.map((s, i) => {
          const r = s.resolved ?? resolveSection(doc, i);
          const span = spans[i];
          const auto = r.autorelease.value;
          const narrow = span.bars * zoom < 70;
          const cls = [
            "sec",
            picked.has(i) && "sel",
            !auto && "loopsec",
            sounding && run.section === i && "cur",
            run.armedSection === i && run.armed && "armed",
            errs.sections.has(i) && "err",
            moveTarget?.from === i && "lifted",
          ]
            .filter(Boolean)
            .join(" ");
          const title = errs.sections.get(i) ?? (run.playing ? `Jump to “${s.id}”` : `Section ${i + 1}: ${s.id} — drag to move, drag the right edge to change its length`);
          return (
            <div
              key={i}
              className={cls}
              style={{ "--s": span.start, "--n": span.bars } as React.CSSProperties}
              data-section={i}
              data-midi={gotoAddress(i)}
              title={title}
              onPointerDown={(e) => onMarkerDown(e, i)}
              onDoubleClick={() => editable && setRenaming({ kind: "section", index: i })}
            >
              <div className="t">
                <span className="no">{i + 1}</span>
                {renaming?.kind === "section" && renaming.index === i ? (
                  <RenameInput value={s.id} onDone={commitRename} taken={doc.sections.map((x) => x.id).filter((x) => x !== s.id)} what="section" />
                ) : (
                  <span className="id">{s.id}</span>
                )}
                {s.repeat !== undefined && <i className="ic ic-link" style={{ "--ic": "11px", color: "var(--accent)" } as React.CSSProperties} />}
                {errs.sections.has(i) && <i className="ic ic-warn" style={{ "--ic": "11px", color: "var(--err)" } as React.CSSProperties} />}
              </div>
              <div className="m">
                <span>{span.bars} {narrow ? "" : span.bars === 1 ? "bar" : "bars"}</span>
                {auto ? (
                  !narrow && (
                    <span>
                      <i className="ic ic-auto" />
                      auto
                    </span>
                  )
                ) : (
                  <>
                    <span className="rel">
                      <i className="ic ic-hand" />
                      {narrow ? "" : "Release"}
                    </span>
                    {!narrow && (
                      <span>
                        <i className={`ic ${r.quantize.value === "loop" ? "ic-loop" : "ic-bar"}`} />
                        {r.quantize.value === "loop" ? "Loop" : "Bar"}
                      </span>
                    )}
                  </>
                )}
              </div>
              {editable && <div className="edge" onPointerDown={(e) => onEdgeDown(e, i)} title="Drag to change the length" />}
            </div>
          );
        })}
        {moveTarget && moveTarget.to !== moveTarget.from && (
          <div
            className="dropline"
            style={{ "--s": moveTarget.to > moveTarget.from ? spans[moveTarget.to].start + spans[moveTarget.to].bars : spans[moveTarget.to].start } as React.CSSProperties}
          />
        )}
        <div className="bars">
          {numbers.map((n) => (
            <span key={n.bar} className={n.strong ? "s" : undefined} style={{ left: `calc(var(--x0) + ${n.bar} * var(--b))` }}>
              {n.bar + 1}
            </span>
          ))}
        </div>
        <div className="ticks" style={{ right: "auto", width: `calc(${total} * var(--b) + 1px)` }} />
        <div className={`playhead ruler-ph${sounding ? "" : " stopped"}`} style={{ "--pos": pos } as React.CSSProperties} />
      </div>

      {/* track heads */}
      <div className="heads" ref={headsRef} onPointerMove={onHeadMove} onPointerUp={onHeadUp} onPointerCancel={() => setDrag(null)}>
        {doc.tracks.map((t, i) => (
          <TrackHead
            key={i}
            track={t}
            index={i}
            selected={sel.kind === "track" && sel.index === i}
            lifted={trackTarget?.from === i}
            dropBefore={trackTarget !== null && trackTarget.to === i && trackTarget.to < trackTarget.from}
            dropAfter={trackTarget !== null && trackTarget.to === i && trackTarget.to > trackTarget.from}
            error={errs.tracks.get(i)}
            editable={editable}
            runtime={engineTracks[t.name]}
            renaming={renaming?.kind === "track" && renaming.index === i}
            taken={doc.tracks.map((x) => x.name).filter((x) => x !== t.name)}
            onRename={commitRename}
            onStartRename={() => editable && setRenaming({ kind: "track", index: i })}
            onPointerDown={(e) => onHeadDown(e, i)}
            edit={edit}
          />
        ))}
        <div className="addtrack">
          <button
            className="btn s"
            disabled={!editable || doc.tracks.length >= MAX_TRACKS}
            onClick={() => {
              edit((d) => ops.addTrack(d));
              setSel({ kind: "track", index: doc.tracks.length });
            }}
          >
            <i className="ic ic-plus" />
            Add track
          </button>
          <span className="dim2" style={{ fontSize: 11 }}>
            {MAX_TRACKS - doc.tracks.length > 0 ? `${MAX_TRACKS - doc.tracks.length} free` : "all 8 in use"}
          </span>
        </div>
      </div>

      {/* lanes */}
      <div className="lanes">
        {[...picked].map((i) =>
          spans[i] ? <div key={`sel${i}`} className="secsel" style={{ "--s": spans[i].start, "--n": spans[i].bars } as React.CSSProperties} /> : null,
        )}
        {spans.slice(1).map((s, i) => (
          <div key={`l${i}`} className="secline" style={{ "--s": s.start } as React.CSSProperties} />
        ))}
        {sounding && <div className="donezone" style={{ "--pos": pos } as React.CSSProperties} />}
        <div className="endzone" style={{ "--s": total } as React.CSSProperties} />
        {editable && (
          <button
            className="addsec"
            style={{ "--s": total } as React.CSSProperties}
            title="Append a section"
            onClick={() => {
              edit((d) => ops.addSection(d));
              setSel({ kind: "section", index: doc.sections.length });
            }}
          >
            <i className="ic ic-plus" />
          </button>
        )}

        {doc.tracks.map((t, ti) => (
          <div key={ti} className={`lane${ti % 2 ? " alt" : ""}${sel.kind === "track" && sel.index === ti ? " sel" : ""}`}>
            {doc.sections.map((s, si) => {
              const r = s.resolved ?? resolveSection(doc, si);
              const st = r.tracks[ti] ?? { state: "stop" as const, from: { kind: "default" as const } };
              const look = STATE_LOOK[st.state];
              const span = spans[si];
              const px = span.bars * zoom;
              const layer = layers[si]?.[ti];
              const inherited = st.from.kind === "repeat";
              const overridden = s.repeat !== undefined && st.from.kind === "own";
              const err = errs.cells.get(`${si}:${t.name}`);
              let lay = "";
              if (st.state === "record") lay = layer?.stacked ? "Layer 1" : `${span.bars} bars`;
              else if (st.state === "overdub" && layer?.writes) lay = `Layer ${layer.writes}`;
              else if (st.state === "play" && layer) lay = layer.layers > 1 ? `${layer.layers} layers` : layer.layers === 0 ? "nothing yet" : "";
              const cls = [
                "clip",
                look.cls,
                inherited && "inh",
                px < 34 ? "tiny" : px < 96 ? "mini" : "",
                cellSel.has(`${si}:${ti}`) && "sel",
                err && "err",
              ]
                .filter(Boolean)
                .join(" ");
              const tip =
                err ??
                `${t.name} · ${s.id}: ${look.word}` +
                  (inherited && st.from.kind === "repeat" ? ` (from ${st.from.section})` : overridden ? " (overrides the template)" : st.from.kind === "default" ? " (not stated: stopped)" : "");
              return (
                <div
                  key={si}
                  className={cls}
                  style={{ "--s": span.start, "--n": span.bars } as React.CSSProperties}
                  data-cell={`${si}:${ti}`}
                  title={tip}
                  onPointerDown={(e) => {
                    if (e.button !== 0) return;
                    setSel(clickCell(sel, { section: si, track: ti }, e.shiftKey, e.metaKey || e.ctrlKey));
                  }}
                >
                  <div className="hd">
                    <i className={`ic ${err ? "ic-warn" : look.icon}`} />
                    <span>{look.word}</span>
                    {lay ? <span className="lay">{lay}</span> : inherited ? <i className="ic ic-link lay" style={{ "--ic": "10px" } as React.CSSProperties} /> : null}
                  </div>
                  {st.state !== "stop" && <div className="bd">{overridden && px >= 96 && <span className="ovr">overridden</span>}</div>}
                  {st.state === "stop" && overridden && px >= 96 && (
                    <div className="bd">
                      <span className="ovr">overridden</span>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        ))}
        {armPos !== null && <div className="armline" style={{ "--pos": armPos } as React.CSSProperties} />}
        <div className={`playhead${sounding ? "" : " stopped"}`} style={{ "--pos": pos } as React.CSSProperties} />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------

function RenameInput({ value, onDone, taken, what }: { value: string; onDone: (v: string) => void; taken: string[]; what: "section" | "track" }) {
  const [text, setText] = useState(value);
  const problem = text === value ? null : nameProblem(text, taken, what);
  return (
    <input
      className={`rename${problem ? " bad" : ""}`}
      autoFocus
      value={text}
      title={problem ?? "Enter to rename, Esc to cancel"}
      onChange={(e) => setText(e.target.value)}
      onPointerDown={(e) => e.stopPropagation()}
      onFocus={(e) => e.currentTarget.select()}
      onBlur={() => onDone(problem ? value : text)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter" && !problem) onDone(text);
        if (e.key === "Escape") onDone(value);
      }}
    />
  );
}

interface HeadProps {
  track: DocTrack;
  index: number;
  selected: boolean;
  lifted: boolean;
  dropBefore: boolean;
  dropAfter: boolean;
  error?: string;
  editable: boolean;
  runtime?: { state: string; layers: number; channels: number };
  renaming: boolean;
  taken: string[];
  onRename: (v: string) => void;
  onStartRename: () => void;
  onPointerDown: (e: ReactPointerEvent) => void;
  edit: (op: (d: ScoreDoc) => ScoreDoc) => void;
}

function TrackHead(p: HeadProps) {
  const t = p.track;
  const monitor = t.monitor !== false;
  const bus = t.bus ?? "main+monitor";
  const toMain = bus === "main" || bus === "main+monitor";
  const toPhones = bus === "monitor" || bus === "main+monitor";
  const setBus = (main: boolean, phones: boolean) =>
    p.edit((d) => ops.patchTrack(d, p.index, { bus: main && phones ? undefined : main ? "main" : phones ? "monitor" : "none" }));
  const rt = p.runtime ? RUNTIME[p.runtime.state] : undefined;
  const cls = ["th", p.selected && "sel", p.lifted && "lifted", p.dropBefore && "drop-before", p.dropAfter && "drop-after", p.error && "err"]
    .filter(Boolean)
    .join(" ");
  return (
    <div className={cls} style={{ "--c": `var(--trk-${(p.index % 8) + 1})` } as React.CSSProperties} onPointerDown={p.onPointerDown} title={p.error} data-track={p.index}>
      <div className="clr" />
      <div className="body">
        <div className="name">
          <span className="idx">{p.index + 1}</span>
          {p.renaming ? (
            <RenameInput value={t.name} onDone={p.onRename} taken={p.taken} what="track" />
          ) : (
            <b onDoubleClick={p.onStartRename}>{t.name}</b>
          )}
          <span className="ch">{t.input_right ? "stereo" : "mono"}</span>
          {p.error && <i className="ic ic-warn" style={{ "--ic": "12px", color: "var(--err)" } as React.CSSProperties} />}
          <div className="grow" />
          {rt && (
            <span className={`st ${rt.cls}`} title="What the engine's track is doing now">
              <i className={`ic ${rt.icon}`} />
              {rt.word}
              {p.runtime && p.runtime.layers > 1 ? ` · ${p.runtime.layers}` : ""}
            </span>
          )}
        </div>
        <div className="ctl">
          <span className="chip in" title="Interface input">
            <i className="ic ic-input" />
            {inputLabel(t)}
          </span>
          <button
            className={`chip hear${monitor ? " on" : ""}`}
            disabled={!p.editable}
            title={monitor ? "Monitoring allowed: you can hear this input live (needed for Record, Overdub and Monitor)" : "Monitoring off for this track"}
            onClick={() => p.edit((d) => ops.patchTrack(d, p.index, { monitor: monitor ? false : undefined }))}
          >
            <i className="ic ic-hear" />
            Monitor
          </button>
          <div className="grow" />
          <button className={`chip bus sq${toMain ? " on" : ""}`} disabled={!p.editable} title={`Loop on main (room): ${toMain ? "on" : "off"}`} onClick={() => setBus(!toMain, toPhones)}>
            <i className="ic ic-speaker" />
          </button>
          <button className={`chip bus sq${toPhones ? " on" : ""}`} disabled={!p.editable} title={`Loop on monitor (headphones): ${toPhones ? "on" : "off"}`} onClick={() => setBus(toMain, !toPhones)}>
            <i className="ic ic-phones" />
          </button>
        </div>
        <div className="meterline">
          <HeadMeter name={t.name} stereo={!!t.input_right} />
          <PanKnob value={t.pan ?? 0} editable={p.editable} onChange={(v) => p.edit((d) => ops.patchTrack(d, p.index, { pan: v === 0 ? undefined : v }))} />
        </div>
      </div>
    </div>
  );
}

/** Output level of the engine track with this name, written straight into the DOM. */
function HeadMeter({ name, stereo }: { name: string; stereo: boolean }) {
  const bars = useRef<(HTMLElement | null)[]>([]);
  const db = useRef<HTMLSpanElement>(null);
  useStatusEffect((s) => {
    const t = s.tracks.find((x) => x.name === name);
    const peaks = t ? t.output_peaks : [];
    bars.current.forEach((el, i) => {
      if (!el) return;
      const peak = peaks[Math.min(i, peaks.length - 1)] ?? 0;
      const dbv = peak > 0 ? 20 * Math.log10(peak) : -Infinity;
      el.style.setProperty("--l", `${Math.max(0, Math.min(100, ((dbv + 60) / 60) * 100))}%`);
    });
    if (db.current) {
      const v = t?.output_dbfs;
      db.current.textContent = v === null || v === undefined || !Number.isFinite(v) ? "−∞" : `${Math.round(v)}`;
    }
  });
  return (
    <>
      <span className="hm">
        {(stereo ? [0, 1] : [0]).map((i) => (
          <i key={i} ref={(el) => (bars.current[i] = el)} style={{ "--l": "0%", "--p": "0%" } as React.CSSProperties} />
        ))}
      </span>
      <span className="db">−∞</span>
    </>
  );
}

/** Pan as the mockup's knob: drag up/down, double-click for centre. */
function PanKnob({ value, editable, onChange }: { value: number; editable: boolean; onChange: (v: number) => void }) {
  const [live, setLive] = useState<number | null>(null);
  const start = useRef<{ y: number; v: number } | null>(null);
  const shown = live ?? value;
  const label = shown === 0 ? "C" : shown < 0 ? `L${Math.round(-shown * 100)}` : `R${Math.round(shown * 100)}`;
  return (
    <span
      className={`knob s${editable ? " pan-live" : ""}`}
      style={{ "--v": shown } as React.CSSProperties}
      title={`Pan ${label}${editable ? " — drag up/down, double-click to centre" : ""}`}
      onPointerDown={(e) => {
        if (!editable) return;
        e.stopPropagation();
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        start.current = { y: e.clientY, v: value };
      }}
      onPointerMove={(e) => {
        if (!start.current) return;
        const v = Math.max(-1, Math.min(1, start.current.v + (start.current.y - e.clientY) / 100));
        setLive(Math.round(v * 20) / 20);
      }}
      onPointerUp={() => {
        if (start.current && live !== null && live !== value) onChange(live);
        start.current = null;
        setLive(null);
      }}
      onDoubleClick={() => editable && value !== 0 && onChange(0)}
    />
  );
}
