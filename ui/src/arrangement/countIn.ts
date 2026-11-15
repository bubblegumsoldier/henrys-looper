//! The score's count-in: one number, two places to set it.
//!
//! ```text
//!   Arrangement field ──► docStore.edit(count_in:) ──► file (⌘S)   ──► "Load" uses it
//!                    └──► runner (if this document is the loaded one and it has not started)
//!   Live button ───────► runner (this run)
//!                    └──► docStore.edit(count_in:)  (if this document is the loaded one)
//! ```
//!
//! The score's `count_in:` is the song's setting; the runner's count-in is what the next start
//! plays. They are kept equal wherever that is unambiguous - the document open in the arrangement
//! is the one the runner holds (`isLive`, the same rule the mixer write-back follows) - so a change
//! made on stage ends up in the file, and a change in the arrangement does not need a reload. With
//! another document open, the live button changes only the run, and the field only the document.
//!
//! The free looper's count-in (Off/1/2 in the LOOP panel) is a different setting and is not touched.

import { scoreApi } from "../api";
import { fail, note } from "../shell/notes";
import { currentStatus } from "../status";
import { isLive } from "../mixer/scoreMix";
import * as ops from "./ops";
import { docStore } from "./useDoc";
import { clampCountIn } from "./countInValue";

export { DEFAULT_COUNT_IN, MAX_COUNT_IN, clampCountIn, docCountIn, nextCountIn, withoutCountIn } from "./countInValue";

/** The open document is the score in the runner, and that score has not started. */
function liveAndIdle(): boolean {
  const state = docStore.get();
  const status = currentStatus();
  return status.score?.phase === "idle" && isLive(state.doc, state.docId, status);
}

/** From the arrangement: the document's `count_in:` (an undo step), and the runner too when it
 *  holds this very document and has not started. */
export function setDocCountIn(bars: number): void {
  const v = clampCountIn(bars);
  const tellRunner = liveAndIdle();
  if (!docStore.edit((d) => ops.setCountIn(d, v))) return;
  if (tellRunner) {
    scoreApi.countIn(v).then(
      (text) => note(text),
      (e) => fail((e as Error).message),
    );
  }
}

/** From the Live view: the runner's count-in for this run, and the document's `count_in:` when the
 *  open document is the one that was loaded. */
export async function setRunCountIn(bars: number): Promise<void> {
  const v = clampCountIn(bars);
  const writeDoc = liveAndIdle();
  try {
    note(await scoreApi.countIn(v));
  } catch (e) {
    fail((e as Error).message);
    return;
  }
  if (writeDoc) docStore.edit((d) => ops.setCountIn(d, v));
}
