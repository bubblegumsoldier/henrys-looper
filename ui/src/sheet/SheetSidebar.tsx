//! The sheet beside the Live stage: a fixed NOW / NEXT strip on top - what is being recorded or
//! played now, and what comes next in how many bars - and below it the whole sheet, the current
//! line highlighted with its progress and the chord that sounds, past lines dimmed, scrolled so the
//! current line stays around the middle.
//!
//! ```text
//!  ┌ Sheet ─────────── A− A+  × ┐
//!  │ NOW   REC bass · 2 bars ●○○○│
//!  │ NEXT  in 3 bars · Sing     │
//!  ├────────────────────────────┤
//!  │ ── Verse 1 ─ ─ ─ ─ ─ ─ ─ ─ │
//!  │ [Am]          [C/G]        │
//!  │▌Walking down the river road│  ← current line, progress underneath
//!  └────────────────────────────┘
//! ```
//!
//! Rendering discipline, as in `LiveView`: the page is drawn once per sheet (`SheetView`); every
//! frame only classes, a progress value and two strings are written into the DOM. Between two
//! status events (50 ms apart) the position is extrapolated at the tempo, so a highlight changes on
//! the beat rather than a status tick late. Scrolling by hand pauses the auto-scroll for a few
//! seconds.
//!
//! L toggles the sidebar (`LiveView`), A−/A+ set the font size, the left edge drags the width; all
//! three are remembered (`layout.ts`).

import { useEffect, useRef, type CSSProperties } from "react";
import { currentStatus, subscribe } from "../status";
import { T } from "../shell/terms";
import { Icon } from "../shell/Icon";
import type { CompiledSection, CompiledSheet } from "../types";
import { SHEET_WIDTH_MAX, SHEET_WIDTH_MIN, setSheetLayout, toggleSheet, useSheetLayout } from "./layout";
import { useLiveSheet } from "./liveSheet";
import { inBarsText, runPoint, sheetAt, sheetState, type RunPoint } from "./position";
import { SheetView } from "./SheetView";
import "./sheet.css";

/** How long a scroll by hand keeps the auto-scroll away. */
const MANUAL_SCROLL_MS = 4000;
/** Never extrapolate further than this past the last status: a stalled engine stops the sheet. */
const MAX_AHEAD_S = 0.25;

export function SheetSidebar({ onOpenArrangement }: { onOpenArrangement: () => void }) {
  const layout = useSheetLayout();
  const { sheet, sections } = useLiveSheet();

  if (!layout.open) {
    return (
      <button className="sheet-rail" onClick={toggleSheet} title="Show the sheet: lyrics, chords, stage directions · L">
        <Icon name="sheet" />
        <span>Sheet</span>
        <kbd>L</kbd>
      </button>
    );
  }

  const style = { "--sheet-w": `${layout.width * 100}vw`, "--sheet-fs": `${layout.fontSize}px` } as CSSProperties;
  return (
    <aside className="sheet-side" style={style} aria-label="Sheet">
      <Resizer />
      <header className="sheet-head">
        <Icon name="sheet" />
        <b>Sheet</b>
        <span className="grow" />
        <button className="sheet-btn" onClick={() => setSheetLayout({ fontSize: layout.fontSize - 2 })} title="Smaller text" aria-label="Smaller text">
          A−
        </button>
        <button className="sheet-btn" onClick={() => setSheetLayout({ fontSize: layout.fontSize + 2 })} title="Larger text" aria-label="Larger text">
          A+
        </button>
        <button className="sheet-btn" onClick={toggleSheet} title="Hide the sheet · L" aria-label="Hide the sheet">
          <Icon name="x" />
        </button>
      </header>
      {sheet && sections ? (
        <SheetLive sheet={sheet} sections={sections} />
      ) : (
        <div className="sheet-empty">
          <b>{sections ? "This score has no sheet." : "No score loaded."}</b>
          <p>
            A sheet is the song's lyrics, chords and your stage directions, running along with the
            score. Write or paste it in{" "}
            <button className="linkish" onClick={onOpenArrangement}>
              {T.arrangement} › Sheet
            </button>
            :
          </p>
          <pre>{EXAMPLE}</pre>
          <p className="dim">
            <code># Part @section</code> ties a part to a section · <code>&gt; REC …</code> shows red ·{" "}
            <code>|2</code> at the end = lasts 2 bars · chords above the words or inline as <code>[Am]</code>.
          </p>
        </div>
      )}
    </aside>
  );
}

const EXAMPLE = `# Verse @verse
> REC bass · 2 bars |2
Am          C/G
Words of the first line
[F]Second line with [C]chords`;

/** Drag the left edge: the width follows the pointer, as a share of the window. */
function Resizer() {
  return (
    <div
      className="sheet-resize"
      title="Drag to resize"
      onPointerDown={(e) => {
        e.preventDefault();
        const el = e.currentTarget;
        el.setPointerCapture(e.pointerId);
        const move = (ev: PointerEvent) => {
          const w = (window.innerWidth - ev.clientX) / window.innerWidth;
          setSheetLayout({ width: Math.min(SHEET_WIDTH_MAX, Math.max(SHEET_WIDTH_MIN, w)) });
        };
        const up = () => {
          el.removeEventListener("pointermove", move);
          el.removeEventListener("pointerup", up);
          el.removeEventListener("pointercancel", up);
        };
        el.addEventListener("pointermove", move);
        el.addEventListener("pointerup", up);
        el.addEventListener("pointercancel", up);
      }}
    />
  );
}

function SheetLive({ sheet, sections }: { sheet: CompiledSheet; sections: CompiledSection[] }) {
  const scroller = useRef<HTMLDivElement | null>(null);
  const nowRef = useRef<HTMLSpanElement | null>(null);
  const nextRef = useRef<HTMLSpanElement | null>(null);
  const beatsRef = useRef<HTMLSpanElement | null>(null);

  useEffect(() => {
    const box = scroller.current;
    const nowEl = nowRef.current;
    const nextEl = nextRef.current;
    const beatsEl = beatsRef.current;
    if (!box || !nowEl || !nextEl || !beatsEl) return;
    const els: (HTMLElement | undefined)[] = [];
    box.querySelectorAll<HTMLElement>("[data-i]").forEach((el) => {
      els[Number(el.dataset.i)] = el;
    });

    let point: RunPoint = runPoint(currentStatus());
    let t0 = performance.now();
    let raf = 0;
    let lastKey = "";
    let lastCur = -1;
    let lastChord = -1;
    let lastFocus = -2;
    let lastNow = "";
    let lastNext = "";
    let lastBeat = -2;
    let lastBpb = 0;
    let manualUntil = 0;
    let resume: number | undefined;

    const setText = (el: HTMLElement, text: string, rec: boolean, last: string): string => {
      const key = `${rec ? 1 : 0}${text}`;
      if (key === last) return last;
      el.textContent = text;
      el.classList.toggle("rec", rec);
      return key;
    };

    const frame = () => {
      raf = 0;
      const playing = point.phase === "count_in" || point.phase === "running";
      const dt = playing ? Math.min(MAX_AHEAD_S, (performance.now() - t0) / 1000) : 0;
      const at = sheetAt(sheet, sections, point, dt);
      const st = sheetState(sheet, at);

      // Classes: only when the current item or the directions in force change.
      const key = `${st.current}|${st.pastBefore}|${st.active.join(",")}`;
      if (key !== lastKey) {
        lastKey = key;
        sheet.items.forEach((item, i) => {
          const el = els[i];
          if (!el || item.kind === "part") return;
          const active = st.active.includes(i);
          el.classList.toggle("past", i < st.pastBefore && !active);
          el.classList.toggle("cur", i === st.current);
          el.classList.toggle("active", active);
        });
      }
      // Progress of the current item and the chord that sounds.
      if (st.current !== lastCur) {
        const old = els[lastCur];
        old?.querySelector<HTMLElement>(".sh-prog")?.style.setProperty("--p", "0");
        old?.querySelectorAll(".sh-chip.on").forEach((c) => c.classList.remove("on"));
        lastCur = st.current;
        lastChord = -1;
      }
      const cur = els[st.current];
      if (cur) {
        cur.querySelector<HTMLElement>(".sh-prog")?.style.setProperty("--p", st.progress.toFixed(4));
        if (st.chord !== lastChord) {
          cur.querySelectorAll(".sh-chip").forEach((c, k) => c.classList.toggle("on", k === st.chord));
          lastChord = st.chord;
        }
      }

      // NOW / NEXT.
      const now = at === null ? "–" : at.wait > 0 ? T.countIn : (st.now?.text ?? "–");
      lastNow = setText(nowEl, now, at !== null && at.wait === 0 && !!st.now?.rec, lastNow);
      const next = st.next
        ? `${at === null ? "at the start" : inBarsText(st.next.inBars)} · ${st.next.text}`
        : at === null
          ? "–"
          : T.end;
      lastNext = setText(nextEl, next, !!st.next?.rec, lastNext);

      // The beat, extrapolated like the position.
      const bpb = Math.max(1, Math.min(16, point.beatsPerBar));
      if (bpb !== lastBpb) {
        beatsEl.replaceChildren(...Array.from({ length: bpb }, () => document.createElement("i")));
        lastBpb = bpb;
        lastBeat = -2;
      }
      const beat = playing ? Math.floor(point.beat - 1 + point.inBeat + (dt * point.bpm) / 60) % bpb : -1;
      if (beat !== lastBeat) {
        beatsEl.querySelectorAll("i").forEach((d, k) => d.classList.toggle("on", k === beat));
        lastBeat = beat;
      }

      // Keep the current line around the middle - unless the musician scrolled himself.
      let focus = st.current;
      if (focus < 0 && at !== null) {
        focus = sheet.items.findIndex((i) => i.kind !== "part" && i.start >= at.bar - 1e-6);
      }
      if (focus !== lastFocus && performance.now() >= manualUntil) {
        lastFocus = focus;
        const el = els[focus];
        if (!el || at === null) box.scrollTo({ top: 0, behavior: "smooth" });
        else box.scrollTo({ top: Math.max(0, el.offsetTop - box.clientHeight / 2 + el.offsetHeight / 2), behavior: "smooth" });
      }

      if (playing) raf = requestAnimationFrame(frame);
    };
    const kick = () => {
      if (!raf) raf = requestAnimationFrame(frame);
    };

    const unsubscribe = subscribe((s) => {
      point = runPoint(s);
      t0 = performance.now();
      kick();
    });
    const manual = () => {
      manualUntil = performance.now() + MANUAL_SCROLL_MS;
      window.clearTimeout(resume);
      resume = window.setTimeout(() => {
        lastFocus = -2;
        kick();
      }, MANUAL_SCROLL_MS + 50);
    };
    const onPointer = (e: PointerEvent) => {
      // A press on the scrollbar (the box itself), not on a line.
      if (e.target === box) manual();
    };
    const onKey = (e: KeyboardEvent) => {
      if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"].includes(e.key)) manual();
    };
    box.addEventListener("wheel", manual, { passive: true });
    box.addEventListener("touchmove", manual, { passive: true });
    box.addEventListener("pointerdown", onPointer);
    box.addEventListener("keydown", onKey);
    kick();
    return () => {
      unsubscribe();
      cancelAnimationFrame(raf);
      window.clearTimeout(resume);
      box.removeEventListener("wheel", manual);
      box.removeEventListener("touchmove", manual);
      box.removeEventListener("pointerdown", onPointer);
      box.removeEventListener("keydown", onKey);
    };
  }, [sheet, sections]);

  return (
    <>
      <div className="sheet-now">
        <div className="row">
          <span className="k">NOW</span>
          <span className="v" ref={nowRef}>
            –
          </span>
          <span className="sheet-beats" ref={beatsRef} />
        </div>
        <div className="row next">
          <span className="k">NEXT</span>
          <span className="v" ref={nextRef}>
            –
          </span>
        </div>
      </div>
      <div className="sheet-scroll" ref={scroller} tabIndex={0}>
        <SheetView sheet={sheet} />
      </div>
    </>
  );
}
