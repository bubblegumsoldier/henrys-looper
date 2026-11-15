//! The frequency response of a channel's high-pass and three EQ bands, drawn from the very
//! formulas the engine filters with (`engine/src/engine/fx/biquad.rs`: RBJ cookbook, Butterworth
//! high-pass, shelves with slope 1), evaluated on the unit circle at the engine's sample rate.
//!
//! The handles are the controls: drag a band sideways for its frequency and up and down for its
//! gain (Shift for fine), scroll on it for its Q, double-click to flatten it. The high-pass handle
//! moves sideways only. Clicking a handle selects that band for the knobs below the graph.
//!
//! An effect that is off adds nothing to the curve - the line is what the channel sounds like -
//! but its handles stay, dimmed, so it can be set up before it is switched in.

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { fxAddress, fxBandAddress } from "../midi/addresses";
import type { BandKindName, FxStatus } from "../types";
import type { FxActions } from "./fxModel";
import { asDb, asHz, asQ } from "./fxModel";

const W = 280;
const H = 120;
const F_MIN = 20;
const F_MAX = 20000;
/** Visible gain range, +/-. Bands reach 24 dB; the graph is about the musical middle. */
const DB_VIEW = 18;
const SEND_MS = 60;

const xOf = (hz: number) => (Math.log(hz / F_MIN) / Math.log(F_MAX / F_MIN)) * W;
const hzOf = (x: number) => F_MIN * Math.pow(F_MAX / F_MIN, Math.min(W, Math.max(0, x)) / W);
const yOf = (db: number) => H / 2 - (Math.max(-DB_VIEW, Math.min(DB_VIEW, db)) / DB_VIEW) * (H / 2 - 6);
const dbOf = (y: number) => ((H / 2 - y) / (H / 2 - 6)) * DB_VIEW;

type Coeffs = [number, number, number, number, number]; // b0 b1 b2 a1 a2

function norm(b0: number, b1: number, b2: number, a0: number, a1: number, a2: number): Coeffs {
  return [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0];
}

const safeHz = (hz: number, fs: number) => Math.min(Math.max(hz, 10), fs * 0.5 * 0.95);

function highPass(fs: number, hz: number): Coeffs {
  const w0 = (2 * Math.PI * safeHz(hz, fs)) / fs;
  const cos = Math.cos(w0);
  const alpha = Math.sin(w0) / (2 * Math.SQRT1_2);
  return norm((1 + cos) / 2, -(1 + cos), (1 + cos) / 2, 1 + alpha, -2 * cos, 1 - alpha);
}

function band(fs: number, kind: BandKindName, hz: number, q: number, gainDb: number): Coeffs {
  const a = Math.pow(10, gainDb / 40);
  const w0 = (2 * Math.PI * safeHz(hz, fs)) / fs;
  const sin = Math.sin(w0);
  const cos = Math.cos(w0);
  if (kind === "peak") {
    const alpha = sin / (2 * Math.max(q, 0.1));
    return norm(1 + alpha * a, -2 * cos, 1 - alpha * a, 1 + alpha / a, -2 * cos, 1 - alpha / a);
  }
  const alpha = sin * 0.5 * Math.SQRT2;
  const t = 2 * Math.sqrt(a) * alpha;
  if (kind === "high_shelf") {
    return norm(
      a * (a + 1 + (a - 1) * cos + t),
      -2 * a * (a - 1 + (a + 1) * cos),
      a * (a + 1 + (a - 1) * cos - t),
      a + 1 - (a - 1) * cos + t,
      2 * (a - 1 - (a + 1) * cos),
      a + 1 - (a - 1) * cos - t,
    );
  }
  return norm(
    a * (a + 1 - (a - 1) * cos + t),
    2 * a * (a - 1 - (a + 1) * cos),
    a * (a + 1 - (a - 1) * cos - t),
    a + 1 + (a - 1) * cos + t,
    -2 * (a - 1 + (a + 1) * cos),
    a + 1 + (a - 1) * cos - t,
  );
}

/** |H| in dB at `hz`. */
function magDb([b0, b1, b2, a1, a2]: Coeffs, fs: number, hz: number): number {
  const w = (2 * Math.PI * hz) / fs;
  const c1 = Math.cos(w);
  const s1 = Math.sin(w);
  const c2 = Math.cos(2 * w);
  const s2 = Math.sin(2 * w);
  const nr = b0 + b1 * c1 + b2 * c2;
  const ni = -(b1 * s1 + b2 * s2);
  const dr = 1 + a1 * c1 + a2 * c2;
  const di = -(a1 * s1 + a2 * s2);
  return 10 * Math.log10((nr * nr + ni * ni) / Math.max(1e-24, dr * dr + di * di));
}

/** The curve of one chain, in dB at `n` log-spaced points. Exported for the tests. */
export function responseDb(fx: FxStatus, fs: number, n = 140): { hz: number; db: number }[] {
  const on = (name: string) => !fx.bypass && (fx.effects.find((e) => e.name === name)?.on ?? false);
  const filters: Coeffs[] = [];
  if (on("high_pass")) filters.push(highPass(fs, fx.high_pass_hz));
  if (on("eq")) for (const b of fx.bands) if (b.gain_db !== 0) filters.push(band(fs, b.kind, b.hz, b.q, b.gain_db));
  const out: { hz: number; db: number }[] = [];
  for (let i = 0; i <= n; i += 1) {
    const hz = hzOf((i / n) * W);
    let db = 0;
    for (const f of filters) db += magDb(f, fs, Math.min(hz, fs * 0.499));
    out.push({ hz, db });
  }
  return out;
}

/** Which handle is selected: the high-pass, or band 0..2. */
export type EqPick = "hp" | number;

interface Props {
  track: number;
  fx: FxStatus;
  sampleRate: number;
  actions: FxActions;
  pick: EqPick;
  onPick: (p: EqPick) => void;
}

interface Override {
  hp?: number;
  bands: Record<number, { hz?: number; gain?: number; q?: number }>;
}

export function EqGraph({ track, fx, sampleRate, actions, pick, onPick }: Props) {
  const fs = sampleRate > 0 ? sampleRate : 48000;
  const svg = useRef<SVGSVGElement | null>(null);
  // While a handle is held, the graph follows the hand; the engine is told at most every 60 ms.
  const [over, setOver] = useState<Override>({ bands: {} });
  const drag = useRef<{ target: EqPick; x: number; y: number; hz: number; db: number; fine: boolean } | null>(null);
  const pending = useRef<Map<string, () => void>>(new Map());
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flush = () => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    for (const send of pending.current.values()) send();
    pending.current.clear();
  };
  const queue = (key: string, send: () => void) => {
    pending.current.set(key, send);
    if (timer.current === undefined) timer.current = window.setTimeout(flush, SEND_MS);
  };

  // What is drawn: the status, with whatever the hand is holding on top.
  const shown: FxStatus = {
    ...fx,
    high_pass_hz: over.hp ?? fx.high_pass_hz,
    bands: fx.bands.map((b) => {
      const o = over.bands[b.index];
      return o ? { ...b, hz: o.hz ?? b.hz, gain_db: o.gain ?? b.gain_db, q: o.q ?? b.q } : b;
    }),
  };
  const curve = responseDb(shown, fs);
  const path = curve.map((p, i) => `${i ? "L" : "M"}${xOf(p.hz).toFixed(1)} ${yOf(p.db).toFixed(1)}`).join(" ");
  const eqOn = fx.effects.find((e) => e.name === "eq")?.on ?? false;
  const hpOn = fx.effects.find((e) => e.name === "high_pass")?.on ?? false;
  const hpDb = curve.reduce((best, p) => (Math.abs(p.hz - shown.high_pass_hz) < Math.abs(best.hz - shown.high_pass_hz) ? p : best), curve[0]);

  const toLocal = (e: { clientX: number; clientY: number }) => {
    const r = svg.current!.getBoundingClientRect();
    return { x: ((e.clientX - r.left) / r.width) * W, y: ((e.clientY - r.top) / r.height) * H };
  };

  const down = (target: EqPick) => (e: ReactPointerEvent<SVGCircleElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    onPick(target);
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    const p = toLocal(e);
    const b = typeof target === "number" ? shown.bands[target] : null;
    drag.current = {
      target,
      x: p.x,
      y: p.y,
      hz: b ? b.hz : shown.high_pass_hz,
      db: b ? b.gain_db : 0,
      fine: e.shiftKey,
    };
  };

  const move = (e: ReactPointerEvent<SVGCircleElement>) => {
    const d = drag.current;
    if (!d) return;
    const p = toLocal(e);
    if (e.shiftKey !== d.fine) {
      const b = typeof d.target === "number" ? shown.bands[d.target] : null;
      Object.assign(d, { fine: e.shiftKey, x: p.x, y: p.y, hz: b ? b.hz : shown.high_pass_hz, db: b ? b.gain_db : 0 });
      return;
    }
    const k = d.fine ? 0.1 : 1;
    const hz = hzOf(xOf(d.hz) + (p.x - d.x) * k);
    if (d.target === "hp") {
      const v = Math.min(1000, Math.max(20, hz));
      setOver((o) => ({ ...o, hp: v }));
      queue("hp", () => actions.fxSet(track, "high_pass_hz", v));
      return;
    }
    const n = d.target;
    const gain = Math.round(Math.max(-DB_VIEW, Math.min(DB_VIEW, d.db + (dbOf(p.y) - dbOf(d.y)) * k)) * 10) / 10;
    setOver((o) => ({ ...o, bands: { ...o.bands, [n]: { ...o.bands[n], hz, gain } } }));
    queue(`hz${n}`, () => actions.fxSet(track, "band_hz", hz, n));
    queue(`g${n}`, () => actions.fxSet(track, "band_gain_db", gain, n));
  };

  const up = (e: ReactPointerEvent<SVGCircleElement>) => {
    if (!drag.current) return;
    drag.current = null;
    (e.currentTarget as Element).releasePointerCapture(e.pointerId);
    flush();
    // Let the status catch up before letting go of the override, so the handle does not jump back.
    window.setTimeout(() => setOver({ bands: {} }), 250);
  };

  // Scroll on a band handle: its Q. Needs a non-passive listener.
  useEffect(() => {
    const el = svg.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      const t = (e.target as Element).getAttribute?.("data-band");
      if (t === null || t === undefined || t === "hp") return;
      e.preventDefault();
      const n = Number(t);
      const b = fx.bands[n];
      if (!b) return;
      const step = e.shiftKey ? 1.01 : 1.08;
      const q = Math.min(12, Math.max(0.1, e.deltaY < 0 ? b.q * step : b.q / step));
      actions.fxSet(track, "band_q", Math.round(q * 100) / 100, n);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [actions, fx.bands, track]);

  const gridF = [50, 100, 200, 500, 1000, 2000, 5000, 10000];

  return (
    <div className={`eqgraph${fx.bypass ? " byp" : ""}`}>
      <svg ref={svg} viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" aria-label="EQ curve">
        <g className="grid">
          {[-12, -6, 6, 12].map((db) => (
            <line key={db} x1={0} x2={W} y1={yOf(db)} y2={yOf(db)} />
          ))}
          <line className="zero" x1={0} x2={W} y1={yOf(0)} y2={yOf(0)} />
          {gridF.map((f) => (
            <line key={f} x1={xOf(f)} x2={xOf(f)} y1={0} y2={H} />
          ))}
        </g>
        <path className="fill" d={`${path} L${W} ${H} L0 ${H} Z`} />
        <path className="line" d={path} />
        <g className="lbl">
          <text x={xOf(100) + 3} y={H - 4}>
            100
          </text>
          <text x={xOf(1000) + 3} y={H - 4}>
            1k
          </text>
          <text x={xOf(10000) + 3} y={H - 4}>
            10k
          </text>
        </g>
      </svg>
      {/* Handles in HTML over the SVG: a stretched SVG would draw them as ellipses. */}
      <div className="handles">
        <Handle
          x={xOf(shown.high_pass_hz)}
          y={yOf(hpDb?.db ?? 0)}
          on={hpOn && !fx.bypass}
          sel={pick === "hp"}
          label="HP"
          title={`High-pass ${asHz(shown.high_pass_hz)} · drag sideways`}
          address={fxAddress(track, "high_pass.hz")}
          band="hp"
          down={down("hp")}
          move={move}
          up={up}
          reset={() => actions.fxSet(track, "high_pass_hz", 80)}
        />
        {shown.bands.map((b) => (
          <Handle
            key={b.index}
            x={xOf(b.hz)}
            y={yOf(b.gain_db)}
            on={eqOn && !fx.bypass}
            sel={pick === b.index}
            label={String(b.number)}
            title={`Band ${b.number}: ${asHz(b.hz)} · ${asDb(b.gain_db)} · Q ${asQ(b.q)}\nDrag: frequency and gain · scroll: Q · double-click: flat`}
            address={fxBandAddress(track, b.index, "gain")}
            band={String(b.index)}
            down={down(b.index)}
            move={move}
            up={up}
            reset={() => actions.fxSet(track, "band_gain_db", 0, b.index)}
          />
        ))}
      </div>
    </div>
  );
}

function Handle(p: {
  x: number;
  y: number;
  on: boolean;
  sel: boolean;
  label: string;
  title: string;
  address: string;
  band: string;
  down: (e: ReactPointerEvent<SVGCircleElement>) => void;
  move: (e: ReactPointerEvent<SVGCircleElement>) => void;
  up: (e: ReactPointerEvent<SVGCircleElement>) => void;
  reset: () => void;
}) {
  return (
    <span
      className={`eqh${p.on ? "" : " off"}${p.sel ? " sel" : ""}${p.band === "hp" ? " hp" : ""}`}
      style={{ left: `${(p.x / W) * 100}%`, top: `${(p.y / H) * 100}%` }}
      title={p.title}
      data-midi={p.address}
      data-band={p.band}
      onPointerDown={p.down as unknown as React.PointerEventHandler<HTMLSpanElement>}
      onPointerMove={p.move as unknown as React.PointerEventHandler<HTMLSpanElement>}
      onPointerUp={p.up as unknown as React.PointerEventHandler<HTMLSpanElement>}
      onDoubleClick={p.reset}
    >
      <i>{p.label}</i>
    </span>
  );
}
