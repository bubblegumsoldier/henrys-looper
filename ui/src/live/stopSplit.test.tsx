// The stop button of a track tile, split in two: the big part stops at the end of the bar, the
// small red square stops now. And the two keys, S and Shift+S.

import { describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  (globalThis as unknown as { window: unknown }).window ??= globalThis;
});
import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StopSplit } from "./TrackTile";
import { stopKey } from "./stopKeys";
import type { LooperTrackState, LooperTrackStatus } from "../types";

function track(state: LooperTrackState, pending: LooperTrackStatus["pending_kind"] = null): LooperTrackStatus {
  return { index: 2, name: "voice", state, pending_kind: pending } as LooperTrackStatus;
}

type ButtonProps = {
  className: string;
  title: string;
  "data-midi": string;
  "aria-label": string;
  onClick: (e: { stopPropagation: () => void }) => void;
};

/** The two `<button>` elements of the split, big part first. */
function buttons(el: ReactNode): ReactElement<ButtonProps>[] {
  const out: ReactElement<ButtonProps>[] = [];
  const walk = (n: ReactNode) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!isValidElement(n)) return;
    if (n.type === "button") out.push(n as ReactElement<ButtonProps>);
    walk((n.props as { children?: ReactNode }).children);
  };
  walk(el);
  return out;
}

function split(t: LooperTrackStatus, clockRuns = true) {
  const onStop = vi.fn();
  const onStopNow = vi.fn();
  const el = StopSplit({ track: t, clockRuns, onStop, onStopNow });
  const [main, now] = buttons(el);
  return { el, main, now, onStop, onStopNow };
}

describe("the split stop button", () => {
  it("is one group of two buttons: stop at bar end and stop now, each with its own MIDI address", () => {
    const { el, main, now } = split(track("playing"));
    const html = renderToStaticMarkup(el);
    expect(html).toContain('role="group"');
    expect(main.props.className).toContain("sp-main");
    expect(main.props["data-midi"]).toBe("track.3.stop");
    expect(main.props.title).toMatch(/^Stop at bar end · key S/);
    expect(html).toContain("at bar end");
    expect(now.props.className).toContain("sp-now");
    expect(now.props["data-midi"]).toBe("track.3.stop_now");
    expect(now.props["aria-label"]).toBe("Stop now");
    expect(now.props.title).toBe("Stop now (cuts the loop mid-bar) · Shift+S");
  });

  it("calls the bar-end stop from the big part and the instant one from the small part", () => {
    const { main, now, onStop, onStopNow } = split(track("playing"));
    const stopPropagation = vi.fn();
    main.props.onClick({ stopPropagation });
    expect(onStop).toHaveBeenCalledTimes(1);
    expect(onStopNow).not.toHaveBeenCalled();
    now.props.onClick({ stopPropagation });
    expect(onStopNow).toHaveBeenCalledTimes(1);
    expect(onStop).toHaveBeenCalledTimes(1);
    // The tile underneath (select on click) must not see the press twice.
    expect(stopPropagation).toHaveBeenCalledTimes(2);
  });

  it("shows a stop that waits for its bar line as armed, and says that a press takes it back", () => {
    const { el, main } = split(track("playing", "stop"));
    expect(main.props.className).toContain("pend");
    expect(main.props.title).toMatch(/press again to cancel/);
    expect(main.props["aria-label"]).toMatch(/armed/);
    expect(renderToStaticMarkup(el)).toContain("tsplit waits");
    // Not armed: no armed look.
    expect(split(track("playing")).main.props.className).not.toContain("pend");
  });

  it("does not promise a bar line while the clock stands", () => {
    const { el, main } = split(track("playing"), false);
    expect(main.props.title).toMatch(/stops at once/);
    expect(renderToStaticMarkup(el)).toContain("at once");
    expect(renderToStaticMarkup(el)).not.toContain("at bar end");
  });

  it("says what it does to a take: ends it at the bar end, never mid-bar", () => {
    const { main, now } = split(track("recording"));
    expect(main.props.title).toMatch(/take ends at the end of the bar/);
    expect(now.props.title).toMatch(/never cut mid-bar/);
    expect(split(track("overdub", "stop")).main.props.title).toMatch(/let it run on/);
    expect(split(track("armed", "record")).main.props.title).toMatch(/cancelled/);
  });
});

describe("the stop keys", () => {
  it("S stops at the bar end, Shift+S stops now", () => {
    expect(stopKey("s", false, false)).toBe("bar_end");
    expect(stopKey("S", true, false)).toBe("now");
    expect(stopKey("s", true, false)).toBe("now");
  });

  it("leaves Shift+S to the score's Stop all while a score runs", () => {
    expect(stopKey("S", true, true)).toBeNull();
    expect(stopKey("s", false, true)).toBe("bar_end");
  });

  it("is not triggered by other keys", () => {
    for (const k of ["p", "r", "o", "Backspace", " "]) {
      expect(stopKey(k, false, false)).toBeNull();
      expect(stopKey(k, true, false)).toBeNull();
    }
  });
});
