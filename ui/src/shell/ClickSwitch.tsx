//! The click (metronome) switch: one control, the same in every view - big in the title bar next to
//! the Arrangement|Live switch, and as a tile in the Live view's row of main buttons. Key K toggles
//! it everywhere (`useClickKey`), MIDI reaches it through `global.click`, and the state survives a
//! restart (`useRememberClick` writes it into the start configuration).
//!
//! Three looks: on (lit), off (dimmed, the symbol struck through), and "on, but silent right now" -
//! a loaded score keeps its click window closed until it plays, so the switch is on and nothing
//! clicks yet. A right-click, or the small arrow, opens the click volume.

import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { GLOBAL } from "../midi/addresses";
import { currentConfig, setConfig } from "../setup/config";
import { currentStatus, useStatusSlice } from "../status";
import type { LooperStatus } from "../types";
import { Icon } from "./Icon";
import { run } from "./notes";
import { T } from "./terms";

/** 0 = no stage at all, then bit 1 = shown, bit 2 = on, bit 4 = the click can be heard now. */
function pick(s: LooperStatus): number {
  if (!s.ready) return 0;
  // Without an engine nothing clicks, so "on" is all there is to say.
  return 1 | (s.click ? 2 : 0) | (s.click_open || !s.running ? 4 : 0);
}

/**
 * K, the switch, MIDI. Without a running engine the click is a setting of the next start: it goes
 * into the setup, and the resting stage shows it at once - nothing to start for that.
 */
export function toggleClick(): void {
  const s = currentStatus();
  if (!s.ready) return;
  if (!s.running) {
    setConfig((c) => ({ ...c, click: !c.click }));
    return;
  }
  run(() => api.setClick(!s.click));
}

function setClickGain(g: number): void {
  if (currentStatus().running) run(() => api.clickGain(g));
  else setConfig((c) => ({ ...c, click_gain: g }));
}

function gainDb(g: number): string {
  if (g <= 0.0001) return "−∞";
  const db = 20 * Math.log10(g);
  return `${db > 0 ? "+" : ""}${db.toFixed(1)} dB`;
}

function ClickVolume({ onClose }: { onClose: () => void }) {
  const gain = useStatusSlice((s) => s.click_gain);
  const box = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const close = (e: PointerEvent) => {
      if (!box.current?.contains(e.target as Node) && !(e.target as HTMLElement).closest?.(".click-sw")) onClose();
    };
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("keydown", key);
    };
  }, [onClose]);
  return (
    <div className="click-vol" ref={box} role="dialog" aria-label="Click volume">
      <span className="caps">{T.clickVolume}</span>
      <input
        className="range"
        type="range"
        min={0}
        max={2}
        step={0.05}
        value={gain}
        data-midi={GLOBAL.clickGain}
        onChange={(e) => {
          setClickGain(Number(e.target.value));
        }}
        title="The click is on the monitor bus only"
      />
      <span className="num">{gainDb(gain)}</span>
    </div>
  );
}

/** `bar`: the title bar's switch. `tile`: a big button in the Live view's main row. */
export function ClickSwitch({ variant }: { variant: "bar" | "tile" }) {
  const v = useStatusSlice(pick);
  const [vol, setVol] = useState(false);
  const running = v !== 0;
  const on = (v & 2) !== 0;
  const heard = (v & 4) !== 0;
  const waiting = on && !heard;
  const look = !running ? "" : !on ? " off" : waiting ? " on waiting" : " on";
  const title = !running
    ? T.click
    : !on
      ? `${T.clickOff} · key K. Right-click: volume`
      : waiting
        ? `${T.clickWaiting} · key K. Right-click: volume`
        : `${T.clickOn} · key K. On the monitor bus only, never on Main. Right-click: volume`;
  return (
    <span className={`click-sw ${variant === "tile" ? "stage" : "bar"}`}>
      <button
        className={`${variant === "tile" ? "bigbtn" : "btn"} click-btn${look}`}
        data-midi={GLOBAL.click}
        disabled={!running}
        aria-pressed={on}
        onClick={toggleClick}
        onContextMenu={(e) => {
          e.preventDefault();
          if (running) setVol(!vol);
        }}
        title={title}
      >
        <span className={`metro${on ? "" : " struck"}`}>
          <Icon name="metro" />
        </span>
        <span className="lbl">{T.click}</span>
      </button>
      {variant === "bar" && (
        <button className="click-more" disabled={!running} onClick={() => setVol(!vol)} title={T.clickVolume} aria-expanded={vol}>
          <Icon name="chev-d" />
        </button>
      )}
      {vol && running && <ClickVolume onClose={() => setVol(false)} />}
    </span>
  );
}

function typing(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  if (el.tagName === "INPUT" && (el as HTMLInputElement).type === "range") return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable || el.closest(".cm-editor") !== null;
}

/** K toggles the click in every view, unless a text field or the text editor has the focus. */
export function useClickKey(): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey || e.shiftKey || e.repeat) return;
      if (e.key.toLowerCase() !== "k" || typing(e.target)) return;
      e.preventDefault();
      toggleClick();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/**
 * The click's state and volume go into the start configuration whenever the engine reports a new
 * one - from the switch, the key, MIDI or the setup alike - so the next start opens with them.
 */
export function useRememberClick(): void {
  const v = useStatusSlice((s) => (s.running ? `${s.click ? 1 : 0}:${s.click_gain}` : ""));
  useEffect(() => {
    if (v === "") return;
    const [on, gain] = v.split(":");
    const click = on === "1";
    const click_gain = Number(gain);
    const c = currentConfig();
    if (c.click !== click || c.click_gain !== click_gain) setConfig({ ...c, click, click_gain });
  }, [v]);
}
