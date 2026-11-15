import { describe, expect, it } from "vitest";
import {
  CLIP_HOLD_MS,
  METER_FALL_DB_PER_S,
  METER_FLOOR_DB,
  NORMAL,
  PEAK_FALL_DB_PER_S,
  PEAK_HOLD_MS,
  REDUCED,
  STALE_MS,
  clearClip,
  clipLit,
  createMeter,
  dbFraction,
  dbText,
  meterFeed,
  meterTick,
  toDb,
  type MeterState,
} from "./meter";

const lin = (db: number) => Math.pow(10, db / 20);

/** Run the frame loop from `from` to `to` at `fps`, feeding `feed` (linear) every 50 ms. */
function run(m: MeterState, from: number, to: number, fps: number, feed: number | null) {
  const step = 1000 / fps;
  let nextFeed = from;
  for (let t = from; t <= to + 1e-9; t += step) {
    while (feed !== null && nextFeed <= t) {
      meterFeed(m, feed, nextFeed);
      nextFeed += 50;
    }
    meterTick(m, t, NORMAL);
  }
}

describe("dB mapping", () => {
  it("maps -60..0 dBFS linearly in dB onto the bar", () => {
    expect(dbFraction(METER_FLOOR_DB)).toBe(0);
    expect(dbFraction(-30)).toBeCloseTo(0.5);
    expect(dbFraction(0)).toBe(1);
    expect(dbFraction(6)).toBe(1);
    expect(dbFraction(-90)).toBe(0);
  });

  it("converts linear peaks to dB with the floor as the bottom", () => {
    expect(toDb(1)).toBeCloseTo(0);
    expect(toDb(0.5)).toBeCloseTo(-6.02, 1);
    expect(toDb(0)).toBe(METER_FLOOR_DB);
    expect(toDb(1e-6)).toBe(METER_FLOOR_DB);
  });

  it("prints a readout without a jumping minus sign", () => {
    expect(dbText(-12.4)).toBe("−12");
    expect(dbText(METER_FLOOR_DB)).toBe("−∞");
    expect(dbText(2)).toBe("2");
  });
});

describe("ballistics", () => {
  it("rises at once", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-6), 10);
    meterTick(m, 16, NORMAL);
    expect(m.level).toBeCloseTo(-6, 5);
  });

  it("falls at the defined rate, whatever the frame rate", () => {
    const at60 = createMeter(0);
    const at144 = createMeter(0);
    meterFeed(at60, lin(-6), 0);
    meterFeed(at144, lin(-6), 0);
    // Silence follows; the bar falls for one second.
    run(at60, 0, 1000, 60, 0);
    run(at144, 0, 1000, 144, 0);
    expect(at60.level).toBeCloseTo(-6 - METER_FALL_DB_PER_S, 1);
    expect(at144.level).toBeCloseTo(at60.level, 1);
  });

  it("does not fall below the newest value while it is fresh", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-6), 0);
    run(m, 0, 2000, 60, lin(-20));
    expect(m.level).toBeCloseTo(-20, 5);
  });

  it("drops to the floor once the engine stops sending", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-20), 0);
    meterTick(m, STALE_MS - 10, NORMAL);
    expect(m.level).toBeCloseTo(-20, 5);
    meterTick(m, 20000, NORMAL);
    expect(m.level).toBe(METER_FLOOR_DB);
  });

  it("does not lose a short peak between quieter events", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-30), 0);
    meterTick(m, 16, NORMAL);
    meterFeed(m, lin(-3), 50);
    meterTick(m, 50, NORMAL);
    meterFeed(m, lin(-30), 100);
    meterTick(m, 150, NORMAL);
    // The bar is still near the peak, not back at -30.
    expect(m.level).toBeGreaterThan(-3 - 0.1 * METER_FALL_DB_PER_S - 0.01);
  });
});

describe("peak hold", () => {
  it("holds the peak, then lets it sink slowly", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-6), 0);
    run(m, 0, PEAK_HOLD_MS - 20, 60, null);
    expect(m.peak).toBeCloseTo(-6, 5);
    run(m, PEAK_HOLD_MS - 20 + 1000 / 60, PEAK_HOLD_MS + 1000, 60, null);
    expect(m.peak).toBeCloseTo(-6 - PEAK_FALL_DB_PER_S, 0);
    expect(m.peak).toBeGreaterThanOrEqual(m.level);
  });

  it("restarts the hold on a new peak", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-12), 0);
    meterFeed(m, lin(-6), 1000);
    meterTick(m, 1000 + PEAK_HOLD_MS - 10, NORMAL);
    expect(m.peak).toBeCloseTo(-6, 5);
  });

  it("steps down instead of gliding with reduced motion, and falls slower", () => {
    const m = createMeter(0);
    meterFeed(m, lin(-6), 0);
    meterTick(m, 500, REDUCED);
    expect(m.peak).toBeCloseTo(-6, 5);
    expect(m.level).toBeCloseTo(-6 - REDUCED.fall * 0.5, 5);
    expect(REDUCED.fall).toBeLessThan(NORMAL.fall);
    meterTick(m, PEAK_HOLD_MS + 100, REDUCED);
    expect(m.peak).toBe(m.level);
  });
});

describe("clip", () => {
  it("latches an over for the hold time", () => {
    const m = createMeter(0);
    meterFeed(m, 0.5, 0);
    expect(clipLit(m, 0)).toBe(false);
    meterFeed(m, 1.2, 100);
    meterFeed(m, 0.1, 150);
    expect(clipLit(m, 150)).toBe(true);
    expect(clipLit(m, 100 + CLIP_HOLD_MS - 1)).toBe(true);
    expect(clipLit(m, 100 + CLIP_HOLD_MS + 1)).toBe(false);
  });

  it("goes out on a click", () => {
    const m = createMeter(0);
    meterFeed(m, 1, 0);
    expect(clipLit(m, 10)).toBe(true);
    clearClip(m);
    expect(clipLit(m, 10)).toBe(false);
  });
});
