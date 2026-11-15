//! Development only: drive the running app from the shell, for screenshots and checks without a
//! mouse. Loaded by `main.tsx` only under `npm run dev` / `tauri dev`; stripped from every build.
//!
//! Write a JSON file `ui/autopilot.local.json` (git-ignored) with an increasing `seq` and a list of
//! steps; the app polls it twice a second and runs each new `seq` once:
//!
//! ```json
//! {"seq": 3, "steps": [
//!   {"do": "config", "patch": {"click": false, "bus_gain_main": 0.05}},
//!   {"do": "engineStart"},
//!   {"do": "mode", "to": "live"},
//!   {"do": "scoreLoad", "yaml": "title: …"},
//!   {"do": "scoreStart"}, {"do": "wait", "ms": 4000}, {"do": "scoreNext"},
//!   {"do": "track", "action": "record", "track": 0},
//!   {"do": "ui", "detail": {"setup": "audio"}}
//! ]}
//! ```
//!
//! UI-only steps (`mode`, `ui`) are passed to the frame as a `looper-autopilot` window event.

import { invoke } from "@tauri-apps/api/core";
import { api, filesApi, midiApi, scoreApi } from "../api";
import { fail, note } from "../shell/notes";
import { startEngine, stopEngine } from "../shell/engine";
import { currentConfig, setConfig } from "../setup/config";
import { setLoadedScore } from "../shell/scoreModel";
import { setLearnMode, setMidiFocus, setMidiView } from "../midi/store";
import type { MidiProfileAction } from "../midi/types";
import { updateScoreFile } from "../scoreFile";
import { setMixer, type MixerState } from "../mixer/store";

type Step = Record<string, unknown> & { do: string };

let lastSeq = -1;

async function runStep(step: Step): Promise<void> {
  switch (step.do) {
    case "wait":
      await new Promise((r) => setTimeout(r, Number(step.ms ?? 1000)));
      return;
    case "config":
      setConfig({ ...currentConfig(), ...(step.patch as object) });
      return;
    case "engineStart":
      await startEngine(currentConfig());
      return;
    case "engineStop":
      stopEngine();
      return;
    case "scoreLoad": {
      const yaml = String(step.yaml ?? "");
      updateScoreFile({ text: yaml });
      const loaded = await scoreApi.load(yaml);
      setLoadedScore(loaded.score);
      note(loaded.message);
      return;
    }
    case "fileOpen": {
      // What File > Open does once the picker has answered (scoreActions.openPath).
      const opened = await filesApi.open(String(step.path));
      updateScoreFile({ text: opened.yaml, path: opened.path, name: opened.name, savedText: opened.yaml });
      note(`Opened ${opened.name}.`);
      return;
    }
    case "reload":
      window.location.reload();
      return;
    case "scoreStart":
      note(await scoreApi.start());
      return;
    case "scoreNext":
      note(await scoreApi.next());
      return;
    case "scoreStopAll":
      note(await scoreApi.stopAll());
      return;
    case "track": {
      const t = Number(step.track ?? 0);
      const a = String(step.action);
      if (a === "record") await api.trackRecord(t);
      else if (a === "overdub") await api.trackOverdub(t);
      else if (a === "play") await api.trackPlay(t);
      else if (a === "stop") await api.trackStop(t);
      else if (a === "clear") await api.trackClear(t);
      else if (a === "monitor") await api.trackMonitor(t, Boolean(step.on));
      return;
    }
    case "click":
      await api.setClick(Boolean(step.on));
      return;
    case "busGain":
      await api.busGain(step.bus as "main" | "monitor", Number(step.gain));
      return;
    case "shot":
      // WebKit's own snapshot of this page (app/src-tauri/src/devshot.rs).
      await invoke("dev_snapshot", { path: String(step.path) });
      return;
    case "dump": {
      // The page as text (or one element's), plus the notes, into a file in the temp directory:
      // {"do": "dump", "path": "/private/tmp/…/ui.txt", "sel": ".ticker"}
      const els = step.sel ? Array.from(document.querySelectorAll<HTMLElement>(String(step.sel))) : [document.body];
      // A field shows its value, not its (empty) text.
      const shown = (e: HTMLElement) =>
        e instanceof HTMLInputElement || e instanceof HTMLSelectElement ? `[${e.value}]` : e.innerText;
      const text = els.map(shown).join("\n----\n");
      await invoke("dev_dump", { path: String(step.path), text });
      return;
    }
    case "frame": {
      // Pretend a smaller window (e.g. 1024 x 768) without resizing the real one: the frame is laid
      // out at that size in the top-left corner, and the snapshot is cropped afterwards.
      const frame = document.querySelector<HTMLElement>(".window");
      if (frame) {
        frame.style.width = step.w ? `${Number(step.w)}px` : "";
        frame.style.height = step.h ? `${Number(step.h)}px` : "";
      }
      return;
    }
    case "learn":
      setLearnMode(Boolean(step.on));
      return;
    case "midiLearn":
      // What a click on a control does in learn mode: arm it (null cancels).
      setMidiView(await midiApi.learn((step.address as string | undefined) ?? null));
      return;
    case "midiProfile":
      setMidiView(await midiApi.profile(step.action as MidiProfileAction));
      return;
    case "scroll":
      // Bring an element into view inside its scrolling panel: {"do": "scroll", "sel": ".dev", "index": 5}
      document.querySelectorAll<HTMLElement>(String(step.sel))[Number(step.index ?? 0)]?.scrollIntoView({ block: "start" });
      return;
    case "clickSel":
      // A click on the first element matching a CSS selector (a row, a button).
      document.querySelector<HTMLElement>(String(step.sel))?.click();
      return;
    case "clickText": {
      // A hand's click on the control that shows this text: {"do": "clickText", "within": ".drawer",
      // "sel": "button", "text": "MIDI"}. Goes through pointer and mouse events like a real click,
      // so a disabled button stays dead and a covered one is found out (`elementFromPoint`).
      const scope = step.within ? document.querySelector<HTMLElement>(String(step.within)) : document.body;
      const el = Array.from(scope?.querySelectorAll<HTMLElement>(String(step.sel ?? "button")) ?? []).find(
        (e) => e.innerText.trim() === String(step.text),
      );
      if (!el) throw new Error(`no ${String(step.sel ?? "button")} "${String(step.text)}"`);
      el.scrollIntoView({ block: "center" });
      const r = el.getBoundingClientRect();
      const at = { clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
      const top = document.elementFromPoint(at.clientX, at.clientY);
      if (top && top !== el && !el.contains(top)) throw new Error(`"${String(step.text)}" is covered by ${top.className}`);
      const opts = { bubbles: true, cancelable: true, button: 0, pointerId: 1, ...at };
      el.dispatchEvent(new PointerEvent("pointerdown", opts));
      el.dispatchEvent(new MouseEvent("mousedown", opts));
      el.dispatchEvent(new PointerEvent("pointerup", opts));
      el.dispatchEvent(new MouseEvent("mouseup", opts));
      el.dispatchEvent(new MouseEvent("click", opts));
      return;
    }
    case "selectValue": {
      // Pick an option of a <select> the way the browser reports a choice: {"do": "selectValue",
      // "within": ".drawer", "index": 0, "value": "1/16"}.
      const scope = step.within ? document.querySelector<HTMLElement>(String(step.within)) : document.body;
      const sel = scope?.querySelectorAll<HTMLSelectElement>(String(step.sel ?? "select"))[Number(step.index ?? 0)];
      if (!sel) throw new Error(`no select ${String(step.sel ?? "select")}`);
      // React tracks the value itself; set it through the prototype so the change is seen.
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(sel, String(step.value));
      sel.dispatchEvent(new Event("change", { bubbles: true }));
      return;
    }
    case "midiOpen":
      setMidiView(await midiApi.open(String(step.device)));
      return;
    case "midiFocus":
      setMidiFocus((step.address as string | undefined) ?? null);
      return;
    case "mixer":
      // {"do": "mixer", "patch": {"open": true, "selected": 1, "full": false}}
      setMixer(step.patch as Partial<MixerState>);
      return;
    case "dom": {
      // Drive a control like a hand would: {"do": "dom", "sel": ".strip .fader", "index": 0,
      // "events": [["pointerdown", {"clientY": 500}], ["pointermove", {"clientY": 450}], ...]}
      const el = document.querySelectorAll<HTMLElement>(String(step.sel))[Number(step.index ?? 0)];
      if (!el) throw new Error(`no element ${String(step.sel)}`);
      const r = el.getBoundingClientRect();
      for (const [type, init] of step.events as [string, Record<string, unknown>][]) {
        const opts = { bubbles: true, cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, button: 0, pointerId: 1, ...init };
        if (type.startsWith("key")) el.dispatchEvent(new KeyboardEvent(type, opts));
        else if (type.startsWith("pointer")) el.dispatchEvent(new PointerEvent(type, opts));
        else el.dispatchEvent(new MouseEvent(type, opts));
        await new Promise((res) => setTimeout(res, 30));
      }
      return;
    }
    case "invoke":
      // Any Tauri command by name: {"do": "invoke", "cmd": "track_gain", "args": {"track": 0, "gain": 0.5}}
      await invoke(String(step.cmd), (step.args ?? {}) as Record<string, unknown>);
      return;
    case "mode":
    case "ui":
      window.dispatchEvent(new CustomEvent("looper-autopilot", { detail: step }));
      return;
    default:
      fail(`autopilot: unknown step ${step.do}`);
  }
}

async function poll(): Promise<void> {
  try {
    const res = await fetch(`/autopilot.local.json?t=${Date.now()}`, { cache: "no-store" });
    if (res.ok) {
      const job = (await res.json()) as { seq: number; steps: Step[] };
      if (lastSeq === -1) lastSeq = job.seq; // never replay what was there before this page loaded
      else if (job.seq > lastSeq) {
        lastSeq = job.seq;
        for (const step of job.steps) {
          try {
            await runStep(step);
          } catch (e) {
            fail(`autopilot ${step.do}: ${(e as Error).message ?? String(e)}`);
          }
        }
      }
    }
  } catch {
    // No file (the dev server answers with index.html): anything written from now on is new.
    if (lastSeq === -1) lastSeq = 0;
  }
  window.setTimeout(poll, 500);
}

void poll();
