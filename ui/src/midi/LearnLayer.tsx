//! Learn by clicking the control itself, and the badge that says what is on it.
//!
//! # The whole idea in one sentence
//!
//! Turn on "MIDI Learn", click the button you want on a pad, press the pad. No list of a hundred
//! addresses to search through, no manual to look the address up in: the thing you point at *is*
//! the thing you bind.
//!
//! # Why one listener on the document and not a wrapper per control
//!
//! Every learnable element carries its address in a `data-midi` attribute (see `addresses.ts`).
//! This component installs **one** listener in the capture phase, finds the nearest ancestor with
//! that attribute and arms the learn mode with it. That means:
//!
//! * no component had to grow a prop, a wrapper or a callback - a button that gained one attribute
//!   is otherwise the button it was;
//! * a control that is added later becomes learnable by writing that one attribute;
//! * the badge is drawn by CSS from the same attribute, so a changed mapping renders nothing.
//!
//! # Why a switch and not only Ctrl
//!
//! Both work, and the switch is the one that is offered. With Ctrl held one clicks next to the
//! button often enough, and next to "Record" is "Record": a missed modifier would start a take
//! instead of binding a pad. That is a bad trade on a stage, so the reliable way is the visible one
//! and Ctrl is the shortcut for whoever wants it.
//!
//! On the Mac the shortcut is ⌘ instead: Ctrl+click is a right click there.
//!
//! # Disabled controls are learnable too
//!
//! "Play" is disabled on an empty track, and the transport buttons are disabled while no score
//! runs - exactly the moments one sets a controller up. A disabled `<button>` gets no click, and in
//! WebKit (the Mac's web view) no pointer event at all. So while the mode is on, the CSS lets
//! pointer events fall through a disabled learnable control (`pointer-events: none`), the event
//! lands on its parent, and [`learnableAt`] finds the disabled control under the pointer by its
//! rectangle - only among the descendants of the element that was actually hit, so a panel lying
//! over a control can never hand the click to what is underneath.
//!
//! # The badge, and whether the binding works
//!
//! Every element whose address is bound gets `data-midi-key` (`Pad 36`, or `Pad 36 · FS 1` when two
//! controls do the same thing) and `data-midi-state`: `active` when at least one of them works right
//! now, `inactive` when none does (device unplugged, track not in this session, taken over by the
//! score - `bindings[].reason` in the view says which). The CSS strikes an inactive badge through.
//!
//! # Why the click is swallowed
//!
//! In learn mode a click on a learnable control must not *also* do its job - clicking "Record" to
//! bind it would otherwise arm a take. The interception happens in the capture phase and on
//! `pointerdown`, because a slider acts while the finger is down and a `click` handler would be too
//! late. Anything **without** an address is left alone, which is what keeps the panel's own buttons
//! (including the switch that turns this off again) working while the mode is on.

import { useEffect } from "react";
import { modHeld } from "../platform";
import { midiApi } from "../api";
import {
  bindingsFor,
  isLearnMode,
  loadMidiView,
  setLearnMode,
  setMidiFocus,
  setMidiView,
  sayMidi,
  useLearnMode,
  useMidiFocus,
  useMidiView,
} from "./store";
import { M } from "./labels";
import { bindingWorks } from "./types";

/** How long a `pointerdown` claims the `click` that follows it, so one gesture arms once. */
const GESTURE_MS = 1000;

/**
 * The learnable control a pointer event means: the nearest element with `data-midi` around the
 * target, or - for a disabled control the event fell through - the smallest disabled learnable
 * control inside the hit element whose rectangle contains the pointer.
 */
function learnableAt(e: MouseEvent): HTMLElement | null {
  const hit = e.target as HTMLElement | null;
  const direct = hit?.closest?.("[data-midi]") as HTMLElement | null;
  if (direct) return direct;
  if (!hit?.querySelectorAll) return null;
  let best: HTMLElement | null = null;
  let bestArea = Infinity;
  for (const el of hit.querySelectorAll<HTMLElement>(
    "[data-midi]:disabled, [data-midi][aria-disabled='true'], [data-midi]:has(:disabled)",
  )) {
    const r = el.getBoundingClientRect();
    if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) continue;
    const area = r.width * r.height;
    if (area < bestArea) {
      best = el;
      bestArea = area;
    }
  }
  return best;
}

export function LearnLayer(): null {
  const learnMode = useLearnMode();
  const view = useMidiView();
  const focus = useMidiFocus();
  const learning = view.learning;

  // Whatever the Rust side already knows - connections survive a reload of the web view.
  useEffect(loadMidiView, []);

  // --- pointing: a control under the pointer lights up its row in the sidebar ------------------
  useEffect(() => {
    if (!learnMode) return;
    const over = (e: PointerEvent) => {
      const el = (e.target as HTMLElement | null)?.closest?.("[data-midi]") as HTMLElement | null;
      setMidiFocus(el?.dataset.midi ?? null);
    };
    document.addEventListener("pointerover", over);
    return () => {
      document.removeEventListener("pointerover", over);
      setMidiFocus(null);
    };
  }, [learnMode]);

  // --- the interception ----------------------------------------------------
  // Installed once. Everything it needs to decide is read from the store, not from a closure, so a
  // changed learn mode does not mean removing and adding a listener.
  useEffect(() => {
    let claimed: { element: Element; at: number } | null = null;

    const grab = (e: PointerEvent | MouseEvent) => {
      if (!isLearnMode() && !modHeld(e)) return;
      const element = learnableAt(e);
      const address = element?.dataset.midi;
      // No address means this is not a learnable control - the panel, a tab, a text field. It keeps
      // working as it always did, which is what makes the mode leaveable.
      if (!element || !address) return;

      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();

      // A pointer gesture is pointerdown + click. Arm on the first and swallow the second.
      const now = Date.now();
      if (e.type === "click" && claimed && claimed.element === element && now - claimed.at < GESTURE_MS) {
        return;
      }
      claimed = { element, at: now };

      midiApi
        .learn(address)
        .then(setMidiView)
        .catch((error) => sayMidi((error as Error).message));
    };

    document.addEventListener("pointerdown", grab, true);
    document.addEventListener("click", grab, true);
    return () => {
      document.removeEventListener("pointerdown", grab, true);
      document.removeEventListener("click", grab, true);
    };
  }, []);

  // --- Escape ---------------------------------------------------------------
  // First press takes back the pending target, second leaves the mode. Two steps, because "I picked
  // the wrong button" is a different thought from "I am done binding things".
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (learning) {
        e.preventDefault();
        midiApi
          .learn(null)
          .then(setMidiView)
          .catch((error) => sayMidi((error as Error).message));
        return;
      }
      if (isLearnMode()) {
        e.preventDefault();
        setLearnMode(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [learning]);

  // --- the badges -----------------------------------------------------------
  // Every element with an address gets `data-midi-key` when something is bound to it; the CSS draws
  // the little `Pad 36` from that attribute. Done by hand rather than by rendering, because the
  // alternative would be a prop and a render on every control whenever any binding changes.
  useEffect(() => {
    let scheduled = 0;

    const paint = () => {
      scheduled = 0;
      for (const element of document.querySelectorAll<HTMLElement>("[data-midi]")) {
        const address = element.dataset.midi;
        if (!address) continue;
        const bound = bindingsFor(address);
        if (learning === address) {
          // The target: its badge says what it is waiting for (design: `.mb.waitb`).
          element.dataset.midiKey = M.waiting;
          element.dataset.midiState = "waiting";
        } else if (bound.length > 0) {
          element.dataset.midiKey = [...new Set(bound.map((b) => b.short))].join(" · ");
          element.dataset.midiState = bound.some((b) => bindingWorks(b.state)) ? "active" : "inactive";
        } else {
          delete element.dataset.midiKey;
          delete element.dataset.midiState;
        }
        element.classList.toggle("midi-learnable", learnMode);
        element.classList.toggle("midi-waiting", learning === address);
        element.classList.toggle("midi-focus", focus === address && learning !== address);
        element.classList.toggle("midi-from-score", bound.some((b) => b.from_score && bindingWorks(b.state)));
      }
    };

    const schedule = () => {
      if (scheduled === 0) scheduled = window.requestAnimationFrame(paint);
    };

    paint();
    // The track cards are rebuilt whenever the structure changes, and a fresh button carries no
    // badge yet. Watching the tree is cheaper than making every card know about the mapping.
    const observer = new MutationObserver(schedule);
    // `data-midi` itself is watched too: a control may change its address in place (the grid
    // button offers the grid it would switch to), and its badge has to follow.
    observer.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["data-midi"] });
    return () => {
      observer.disconnect();
      if (scheduled !== 0) window.cancelAnimationFrame(scheduled);
    };
  }, [learnMode, learning, view.bindings, focus]);

  return null;
}
