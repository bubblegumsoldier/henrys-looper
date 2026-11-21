import { describe, expect, it } from "vitest";
import {
  ADVICE,
  ALARM_HOLD_MS,
  createLoadMeter,
  loadAdvice,
  loadFeed,
  loadFraction,
  loadLevel,
  loadTick,
  loadTitle,
  percentText,
  resetOverruns,
  sinceReset,
} from "./loadModel";
import { IDLE_LOAD, type LoadStatus } from "../types";

const load = (peak: number, overruns = 0, extra: Partial<LoadStatus> = {}): LoadStatus => ({
  ...IDLE_LOAD,
  peak_percent: peak,
  avg_percent: peak / 2,
  overruns,
  block_frames: 128,
  budget_ms: 2.6667,
  ...extra,
});

describe("load meter", () => {
  it("rises at once and falls slowly, by time", () => {
    const m = createLoadMeter(0);
    loadFeed(m, load(80), 0);
    expect(m.level).toBe(80);
    expect(m.shown).toBe(80);
    loadFeed(m, load(20), 50);
    loadTick(m, 1000);
    // 25 points per second: one second after the peak it reads 55, not 20.
    expect(m.level).toBeCloseTo(55, 0);
    loadTick(m, 5000);
    expect(m.level).toBe(20);
  });

  it("changes the number no more than four times a second", () => {
    const m = createLoadMeter(0);
    loadFeed(m, load(30), 0);
    loadFeed(m, load(60), 100);
    expect(m.shown).toBe(30);
    loadTick(m, 260);
    expect(m.shown).toBe(60);
  });

  it("warns from 70 %, alarms at the budget and for a while after an overrun", () => {
    const m = createLoadMeter(0);
    loadFeed(m, load(40), 0);
    expect(loadLevel(m, 0)).toBe("ok");
    loadFeed(m, load(72), 100);
    expect(loadLevel(m, 100)).toBe("warn");
    loadFeed(m, load(104, 1), 200);
    expect(loadLevel(m, 200)).toBe("alarm");
    // The load is back down, the overrun keeps the alarm for a moment.
    loadFeed(m, load(10, 1), 2000);
    expect(m.level).toBeLessThan(70);
    expect(loadLevel(m, 2000)).toBe("alarm");
    loadFeed(m, load(10, 1), 200 + ALARM_HOLD_MS + 100);
    expect(m.level).toBe(10);
    expect(loadLevel(m, 200 + ALARM_HOLD_MS + 100)).toBe("ok");
  });

  it("counts overruns from the last reset and starts again with a new engine", () => {
    const m = createLoadMeter(0);
    loadFeed(m, load(10, 0), 0);
    expect(sinceReset(m)).toBe(0);
    loadFeed(m, load(10, 3), 100);
    expect(sinceReset(m)).toBe(3);
    resetOverruns(m);
    expect(sinceReset(m)).toBe(0);
    expect(loadLevel(m, 150)).toBe("ok");
    loadFeed(m, load(10, 4), 200);
    expect(sinceReset(m)).toBe(1);
    // The engine was restarted: its total starts at 0 again.
    loadFeed(m, load(10, 0), 300);
    expect(sinceReset(m)).toBe(0);
    loadFeed(m, load(10, 1), 400);
    expect(sinceReset(m)).toBe(1);
  });

  it("maps percent to a bar that is full at the budget and ignores nonsense", () => {
    expect(loadFraction(0)).toBe(0);
    expect(loadFraction(35)).toBeCloseTo(0.35);
    expect(loadFraction(250)).toBe(1);
    expect(loadFraction(NaN)).toBe(0);
    expect(percentText(34.4)).toBe("34 %");
    expect(percentText(1234)).toBe("999+ %");
  });

  it("explains itself and gives the advice only after an overrun", () => {
    expect(loadAdvice(load(50))).toBeNull();
    const quiet = loadTitle(load(50, 0, { plugin_percent: 12 }), 0);
    expect(quiet).toContain("peak 50 %, average 25 % of the 2.67 ms budget");
    expect(quiet).toContain("Plugins use 12 %");
    expect(quiet).toContain("No overruns");
    expect(quiet).not.toContain(ADVICE);

    const bad = load(118, 2, {
      last_overrun: "21:14:05",
      last_overrun_kind: "late block + driver overload",
      last_overrun_percent: 118,
    });
    expect(loadAdvice(bad)).toBe("Try a larger buffer in Setup › Audio.");
    const title = loadTitle(bad, 2);
    expect(title).toContain("2 overruns since the engine started. Last at 21:14:05 (late block + driver overload, 118 %).");
    expect(title).toContain(ADVICE);
    expect(title).toContain("Click to reset");
  });
});
