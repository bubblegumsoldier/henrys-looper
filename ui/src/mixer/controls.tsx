//! The drawn controls of the mixer: a rotary knob, a channel fader and a draggable number.
//!
//! All three share one behaviour (`useDrag`), the one a DAW user's hands already know:
//!
//! * **drag** up and down (a knob over 200 px, a fader exactly under the finger),
//! * **Shift** while dragging, scrolling or pressing an arrow key: ten times finer,
//! * **double-click**, Delete or Backspace: back to the default value,
//! * **keyboard** when focused: arrows step 1 %, Page Up/Down 10 %, Home/End to the ends,
//! * **scroll wheel** steps like an arrow key,
//! * the **value** is always printed next to the control and in its tooltip.
//!
//! What a control travels over is a *position* from 0 to 1; `toPos` and `fromPos` translate to the
//! parameter's own unit (linear, logarithmic, or the fader law of `scale.ts`). The engine gets at
//! most one value per 60 ms while a hand moves the control, and the last one at once when the hand
//! lets go - otherwise the next status event would push an older value back into the control.
//!
//! The `data-midi` address sits on the element that is dragged, so the learn mode arms exactly what
//! was clicked and the badge appears on the control itself (see `midi/addresses.ts`).

import { useCallback, useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";

const SEND_MS = 60;

/** A value that follows the hand and reaches the engine throttled. */
export function useSent(value: number, send: (v: number) => void) {
  const [local, setLocal] = useState(value);
  const active = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  const pending = useRef<number | null>(null);
  const out = useRef(send);
  out.current = send;

  useEffect(() => {
    if (!active.current) setLocal(value);
  }, [value]);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flush = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    if (pending.current !== null) {
      out.current(pending.current);
      pending.current = null;
    }
  }, []);

  const push = useCallback((v: number) => {
    setLocal(v);
    pending.current = v;
    if (timer.current !== undefined) return;
    timer.current = window.setTimeout(() => {
      timer.current = undefined;
      if (pending.current !== null) {
        out.current(pending.current);
        pending.current = null;
      }
    }, SEND_MS);
  }, []);

  const begin = useCallback(() => {
    active.current = true;
  }, []);
  const end = useCallback(() => {
    flush();
    active.current = false;
  }, [flush]);

  return { local, push, flush, begin, end };
}

// ---------------------------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------------------------

export interface Curve {
  toPos: (v: number) => number;
  fromPos: (p: number) => number;
}

export function linear(min: number, max: number): Curve {
  return {
    toPos: (v) => (v - min) / (max - min),
    fromPos: (p) => min + p * (max - min),
  };
}

export function logarithmic(min: number, max: number): Curve {
  const span = Math.log(max / min);
  return {
    toPos: (v) => Math.log(Math.max(v, min) / min) / span,
    fromPos: (p) => min * Math.exp(p * span),
  };
}

const clamp01 = (p: number) => Math.min(1, Math.max(0, p));

// ---------------------------------------------------------------------------------------------
// The shared behaviour
// ---------------------------------------------------------------------------------------------

interface DragProps {
  value: number;
  def: number;
  curve: Curve;
  onChange: (v: number) => void;
  disabled?: boolean;
  /** Pixels for the whole travel; `"element"` uses the element's own size (a fader, a bar). */
  travel?: number | "element";
  /** Which way the hand moves: up/down (knobs, faders) or sideways (horizontal bars). */
  axis?: "y" | "x";
}

function useDrag({ value, def, curve, onChange, disabled, travel = 200, axis = "y" }: DragProps) {
  const sent = useSent(value, onChange);
  const ref = useRef<HTMLElement | null>(null);
  const drag = useRef<{ y: number; x: number; pos: number; px: number; fine: boolean } | null>(null);
  const pos = clamp01(curve.toPos(sent.local));
  const posRef = useRef(pos);
  posRef.current = pos;

  const set = useCallback(
    (p: number) => {
      sent.push(curve.fromPos(clamp01(p)));
    },
    [curve, sent],
  );

  // Wheel needs a non-passive listener to keep the page from scrolling.
  useEffect(() => {
    const el = ref.current;
    if (!el || disabled) return;
    let idle: number | undefined;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const step = (e.shiftKey ? 0.002 : 0.01) * (Math.abs(e.deltaY) > 40 ? 2 : 1);
      sent.begin();
      set(posRef.current + (e.deltaY < 0 ? step : -step));
      window.clearTimeout(idle);
      idle = window.setTimeout(sent.end, 200);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("wheel", onWheel);
      window.clearTimeout(idle);
    };
  }, [disabled, set, sent]);

  const handlers = disabled
    ? {}
    : {
        onPointerDown: (e: React.PointerEvent<HTMLElement>) => {
          if (e.button !== 0) return;
          e.preventDefault();
          (e.currentTarget as HTMLElement).focus();
          try {
            e.currentTarget.setPointerCapture(e.pointerId);
          } catch {
            /* a synthetic pointer has nothing to capture */
          }
          const box = e.currentTarget.getBoundingClientRect();
          const px = travel === "element" ? Math.max(40, (axis === "x" ? box.width : box.height) - 8) : travel;
          drag.current = { y: e.clientY, x: e.clientX, pos: posRef.current, px, fine: e.shiftKey };
          sent.begin();
        },
        onPointerMove: (e: React.PointerEvent<HTMLElement>) => {
          const d = drag.current;
          if (!d) return;
          if (e.shiftKey !== d.fine) {
            // Re-anchor when Shift changes mid-drag, so the control does not jump.
            d.fine = e.shiftKey;
            d.y = e.clientY;
            d.x = e.clientX;
            d.pos = posRef.current;
            return;
          }
          const moved = axis === "x" ? e.clientX - d.x : d.y - e.clientY;
          set(d.pos + (moved / d.px) * (d.fine ? 0.1 : 1));
        },
        onPointerUp: (e: React.PointerEvent<HTMLElement>) => {
          if (!drag.current) return;
          drag.current = null;
          if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
          sent.end();
        },
        onPointerCancel: () => {
          drag.current = null;
          sent.end();
        },
        onDoubleClick: () => {
          sent.begin();
          sent.push(def);
          sent.end();
        },
        onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
          const fine = e.shiftKey ? 0.1 : 1;
          let next: number | null = null;
          switch (e.key) {
            case "ArrowUp":
            case "ArrowRight":
              next = posRef.current + 0.01 * fine;
              break;
            case "ArrowDown":
            case "ArrowLeft":
              next = posRef.current - 0.01 * fine;
              break;
            case "PageUp":
              next = posRef.current + 0.1;
              break;
            case "PageDown":
              next = posRef.current - 0.1;
              break;
            case "Home":
              next = 0;
              break;
            case "End":
              next = 1;
              break;
            case "Delete":
            case "Backspace":
              e.preventDefault();
              e.stopPropagation();
              sent.begin();
              sent.push(def);
              sent.end();
              return;
            default:
              return;
          }
          e.preventDefault();
          e.stopPropagation();
          sent.begin();
          set(next);
        },
        onKeyUp: () => sent.end(),
        onBlur: () => sent.end(),
      };

  return { ref, pos, local: sent.local, handlers };
}

interface ControlProps {
  value: number;
  def: number;
  curve: Curve;
  format: (v: number) => string;
  onChange: (v: number) => void;
  /** The parameter-tree address, for MIDI learn. */
  address?: string;
  /** What the control is, for the tooltip and assistive technology. */
  label: string;
  title?: string;
  disabled?: boolean;
}

const tooltip = (label: string, text: string, title?: string) =>
  `${label}: ${text}${title ? `\n${title}` : ""}\nDrag · Shift for fine · double-click for default`;

// ---------------------------------------------------------------------------------------------
// Knob
// ---------------------------------------------------------------------------------------------

/**
 * A rotary knob (`.knob` of the design). `bipolar` draws the arc from the centre (pan, gain);
 * otherwise from the left end. `value` slot: where the printed value goes - `below` with a caption
 * (`.kn`), `side` beside it (`.panrow`), or `none` when the caller prints it.
 */
export function Knob({
  small,
  bipolar,
  caption,
  show = "below",
  ...p
}: ControlProps & { small?: boolean; bipolar?: boolean; caption?: string; show?: "below" | "side" | "none" }) {
  const d = useDrag({ value: p.value, def: p.def, curve: p.curve, onChange: p.onChange, disabled: p.disabled });
  const text = p.format(d.local);
  const v = d.pos * 2 - 1;
  const knob = (
    <span
      ref={d.ref as React.RefObject<HTMLSpanElement>}
      className={`knob${small ? " s" : ""}${bipolar ? " bi" : " uni"}${p.disabled ? " off" : ""}`}
      style={{ "--v": v.toFixed(3) } as CSSProperties}
      role="slider"
      tabIndex={p.disabled ? -1 : 0}
      aria-label={p.label}
      aria-valuetext={text}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(d.pos * 100)}
      aria-disabled={p.disabled || undefined}
      title={tooltip(p.label, text, p.title)}
      data-midi={p.address}
      {...d.handlers}
    />
  );
  if (show === "none") return knob;
  if (show === "side")
    return (
      <>
        {knob}
        <span className="v">{text}</span>
      </>
    );
  return (
    <div className="kn">
      {knob}
      <span className="v">{text}</span>
      {caption && <label>{caption}</label>}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Fader
// ---------------------------------------------------------------------------------------------

/** A vertical channel fader (`.fader` of the design). The curve is normally the fader law. */
export function Fader(p: ControlProps & { children?: ReactNode }) {
  const d = useDrag({ value: p.value, def: p.def, curve: p.curve, onChange: p.onChange, disabled: p.disabled, travel: "element" });
  const text = p.format(d.local);
  return (
    <div
      ref={d.ref as React.RefObject<HTMLDivElement>}
      className={`fader${p.disabled ? " off" : ""}`}
      role="slider"
      tabIndex={p.disabled ? -1 : 0}
      aria-orientation="vertical"
      aria-label={p.label}
      aria-valuetext={text}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(d.pos * 100)}
      title={tooltip(p.label, text, p.title)}
      data-midi={p.address}
      {...d.handlers}
    >
      <div className="cap" style={{ "--f": `calc(4px + (100% - 8px) * ${d.pos.toFixed(4)})` } as CSSProperties} />
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Draggable number
// ---------------------------------------------------------------------------------------------

/** A value printed as text that is itself the control - for tight places like the click level. */
export function DragNumber(p: ControlProps & { className?: string }) {
  const d = useDrag({ value: p.value, def: p.def, curve: p.curve, onChange: p.onChange, disabled: p.disabled, travel: 150 });
  const text = p.format(d.local);
  return (
    <span
      ref={d.ref as React.RefObject<HTMLSpanElement>}
      className={`dragnum${p.className ? ` ${p.className}` : ""}`}
      role="slider"
      tabIndex={p.disabled ? -1 : 0}
      aria-label={p.label}
      aria-valuetext={text}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(d.pos * 100)}
      title={tooltip(p.label, text, p.title)}
      data-midi={p.address}
      {...d.handlers}
    >
      {text}
    </span>
  );
}

// ---------------------------------------------------------------------------------------------
// Horizontal bar
// ---------------------------------------------------------------------------------------------

/** A level as a horizontal bar (`.hm` of the design) that is dragged sideways - layer volumes, mixes. */
export function HBar(p: ControlProps & { className?: string }) {
  const d = useDrag({ value: p.value, def: p.def, curve: p.curve, onChange: p.onChange, disabled: p.disabled, travel: "element", axis: "x" });
  const text = p.format(d.local);
  return (
    <span
      ref={d.ref as React.RefObject<HTMLSpanElement>}
      className={`hm hbar${p.className ? ` ${p.className}` : ""}`}
      role="slider"
      tabIndex={p.disabled ? -1 : 0}
      aria-label={p.label}
      aria-valuetext={text}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(d.pos * 100)}
      title={tooltip(p.label, text, p.title)}
      data-midi={p.address}
      {...d.handlers}
    >
      <i className="nopeak" style={{ "--l": `${(d.pos * 100).toFixed(1)}%` } as CSSProperties} />
    </span>
  );
}
