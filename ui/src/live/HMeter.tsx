//! The segmented horizontal meter of the design (`.hm` in app.css), one bar per channel. A thin
//! name for the shared `Meter` (design/LevelMeter.tsx), which holds the ballistics of every meter.

import type { CSSProperties } from "react";
import { Meter } from "../design/LevelMeter";
import type { LooperStatus } from "../types";

interface Props {
  /** One linear peak (0..1) per channel, read from the status. */
  pick: (status: LooperStatus) => number[];
  /** How many channels to draw. */
  channels: number;
  className?: string;
  style?: CSSProperties;
}

export function HMeter(props: Props) {
  return <Meter {...props} />;
}
