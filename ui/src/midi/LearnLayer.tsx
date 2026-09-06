//! Learn by clicking the control itself, and the badge that says what is on it.
//!
//! # The whole idea in one sentence
//!
//! Turn on „MIDI lernen“, click the button you want on a pad, press the pad. No list of a hundred
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
//! button often enough, and next to „Aufnahme“ is „Aufnahme“: a missed modifier would start a take
//! instead of binding a pad. That is a bad trade on a stage, so the reliable way is the visible one
//! and Ctrl is the shortcut for whoever wants it.
//!
//! # Why the click is swallowed
//!
//! In learn mode a click on a learnable control must not *also* do its job - clicking „Aufnahme“ to
//! bind it would otherwise arm a take. The interception happens in the capture phase and on
//! `pointerdown`, because a slider acts while the finger is down and a `click` handler would be too
//! late. Anything **without** an address is left alone, which is what keeps the panel's own buttons
//! (including the switch that turns this off again) working while the mode is on.

import { useEffect } from "react";
import { midiApi } from "../api";
import { bindingFor, isLearnMode, setLearnMode, setMidiView, sayMidi, useLearnMode, useMidiView } from "./store";

/** How long a `pointerdown` claims the `click` that follows it, so one gesture arms once. */
const GESTURE_MS = 1000;

export function LearnLayer(): null {
  const learnMode = useLearnMode();
  const view = useMidiView();
  const learning = view.learning;

  // --- the interception ----------------------------------------------------
  // Installed once. Everything it needs to decide is read from the store, not from a closure, so a
  // changed learn mode does not mean removing and adding a listener.
  useEffect(() => {
    let claimed: { element: Element; at: number } | null = null;

    const grab = (e: PointerEvent | MouseEvent) => {
      if (!isLearnMode() && !e.ctrlKey) return;
      const target = e.target as HTMLElement | null;
      const element = target?.closest?.("[data-midi]") as HTMLElement | null;
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
        const bound = bindingFor(address);
        if (bound) element.dataset.midiKey = bound.short;
        else delete element.dataset.midiKey;
        element.classList.toggle("midi-learnable", learnMode);
        element.classList.toggle("midi-waiting", learning === address);
        element.classList.toggle("midi-from-score", bound?.from_score ?? false);
      }
    };

    const schedule = () => {
      if (scheduled === 0) scheduled = window.requestAnimationFrame(paint);
    };

    paint();
    // The track cards are rebuilt whenever the structure changes, and a fresh button carries no
    // badge yet. Watching the tree is cheaper than making every card know about the mapping.
    const observer = new MutationObserver(schedule);
    observer.observe(document.body, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      if (scheduled !== 0) window.cancelAnimationFrame(scheduled);
    };
  }, [learnMode, learning, view.bindings]);

  return null;
}
