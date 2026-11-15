//! The mixer's level meters. The bars are the shared `Meter` (design/LevelMeter.tsx) with its
//! ballistics; only the scale is the mixer's own.
//!
//! The vertical meter uses the fader law (`scale.ts`), so the meter and the dB scale beside it
//! agree: a signal at -12 dBFS lights the meter up to the "12" mark.

import { useRef } from "react";
import { Meter } from "../design/LevelMeter";
import { METER_FLOOR_DB } from "../design/meter";
import { useStatusEffect } from "../status";
import type { LooperStatus } from "../types";
import { dbToPos, peakText } from "./scale";

/** The fader law, with the meter floor as the bottom: below -60 dBFS the bar is empty. */
const faderScale = (db: number) => (db > METER_FLOOR_DB ? dbToPos(db) : 0);

interface Props {
  /** One linear peak per channel. */
  pick: (status: LooperStatus) => number[];
  channels: number;
  /** Wider bars, as on a mono strip in the mockup. */
  wide?: boolean;
}

export function VMeter({ pick, channels, wide }: Props) {
  return <Meter vertical pick={pick} channels={channels} wide={wide} scale={faderScale} />;
}

/**
 * The highest peak since the last reset, as a number - the "−9.2 dB" in a strip foot. Clicking it
 * starts over, as on every console. Turns red above 0 dBFS.
 */
export function PeakHold({ pick }: { pick: (status: LooperStatus) => number }) {
  const el = useRef<HTMLSpanElement | null>(null);
  const max = useRef(0);
  const shown = useRef("");

  useStatusEffect((status) => {
    const v = pick(status);
    if (v > max.current) max.current = v;
    const text = peakText(max.current);
    if (text !== shown.current && el.current) {
      el.current.textContent = text;
      el.current.classList.toggle("over", max.current >= 1);
      shown.current = text;
    }
  });

  return (
    <span
      className="pk"
      ref={el}
      role="button"
      title="Highest peak since the last reset. Click to reset."
      onClick={() => {
        max.current = 0;
      }}
    >
      −∞ dB
    </span>
  );
}
