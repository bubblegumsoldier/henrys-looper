//! The compact clip lamp of the title bar: invisible while every output stays below full scale,
//! red and latched once one went over. Its tooltip says which output, by how much, and which
//! volume to turn down; a click puts it out and opens the engine panel with the output meters.

import { useRef, useState } from "react";
import { useStatusEffect } from "../status";
import { Icon } from "./Icon";
import { NO_CLIP, clipped, latchOutputs, outputClipTitle, type OutputLatch } from "./outputLatch";
import { T } from "./terms";

export function OutputClip({ onOpen }: { onOpen: () => void }) {
  const [latch, setLatch] = useState<OutputLatch>(NO_CLIP);
  const ref = useRef(latch);
  useStatusEffect((status) => {
    const next = latchOutputs(ref.current, status.outputs ?? []);
    if (next !== ref.current) {
      ref.current = next;
      setLatch(next);
    }
  });
  if (!clipped(latch)) return null;
  const reset = () => {
    ref.current = NO_CLIP;
    setLatch(NO_CLIP);
    onOpen();
  };
  return (
    <button className="pill err out-clip" onClick={reset} title={outputClipTitle(latch)} role="status">
      <Icon name="warn" size={11} />
      {T.outputClip} {latch.channels.join("/")}
    </button>
  );
}
