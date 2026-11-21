//! The sheet, drawn: part headers with a dashed rule, stage directions as boxes (red when they
//! record), lines with their chords as chips on the exact monospace column. One renderer for the
//! Live sidebar and the editor's preview.
//!
//! It draws the static page only. What moves - the current line, its progress, the chord that
//! sounds, past lines dimmed - is set from outside on the elements (`data-i` = index into
//! `sheet.items`, classes `cur` / `past` / `active`, `--p` on `.sh-prog`, `on` on a chip), a frame at
//! a time without a render (`SheetSidebar`).

import { memo, type CSSProperties } from "react";
import type { CompiledSheet, SheetChord } from "../types";

/** Chips on their columns; one is pushed right only where it would overlap the one before. */
export function chipColumns(chords: SheetChord[]): number[] {
  const out: number[] = [];
  let end = -1;
  for (const c of chords) {
    const col = Math.max(c.col, end + 1);
    out.push(col);
    end = col + c.name.length + 1;
  }
  return out;
}

function bars(n: number): string {
  const text = String(Math.round(n * 1000) / 1000);
  return `${text} bar${n === 1 ? "" : "s"}`;
}

interface Props {
  sheet: CompiledSheet;
  /** A click on an element, with its 1-based line in the sheet text (the editor jumps there). */
  onLine?: (line: number) => void;
}

export const SheetView = memo(function SheetView({ sheet, onLine }: Props) {
  const click = (line: number) => (onLine ? () => onLine(line) : undefined);
  return (
    <>
      {sheet.items.map((item, i) => {
        if (item.kind === "part") {
          return (
            <div key={i} className="sh-part" data-i={i} onClick={click(item.line)}>
              <span>{item.name}</span>
              {item.section && <span className="sh-anchor">@{item.section}</span>}
            </div>
          );
        }
        if (item.kind === "direction") {
          return (
            <div key={i} className={`sh-dir${item.rec ? " rec" : ""}`} data-i={i} onClick={click(item.line)}>
              {item.bars > 0 && <span className="sh-bars">{bars(item.bars)}</span>}
              {item.text}
              <i className="sh-prog" />
            </div>
          );
        }
        const cols = chipColumns(item.chords);
        return (
          <div key={i} className="sh-ln" data-i={i} onClick={click(item.line)}>
            {item.chords.length > 0 && (
              <div className="sh-ch">
                {item.chords.map((c, k) => (
                  <span key={k} className="sh-chip" style={{ left: `${cols[k]}ch` } as CSSProperties}>
                    <b>{c.name}</b>
                  </span>
                ))}
              </div>
            )}
            {item.text !== "" && <div className="sh-tx">{item.text}</div>}
            <i className="sh-prog" />
          </div>
        );
      })}
    </>
  );
});
