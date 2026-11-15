import { describe, expect, it } from "vitest";
import { NO_CLIP, clipped, latchOutputs, outputClipTitle } from "./outputLatch";
import type { OutputMeter } from "../types";

const out = (channel: number, dbfs: number | null, buses = "Main/Monitor", reduction_db = 0): OutputMeter => ({
  channel,
  peak: dbfs === null ? 0 : 10 ** (dbfs / 20),
  dbfs,
  reduction_db,
  buses,
});

describe("output clip latch", () => {
  it("stays dark below full scale and keeps the same object", () => {
    const next = latchOutputs(NO_CLIP, [out(1, -1.4), out(2, null)]);
    expect(next).toBe(NO_CLIP);
    expect(clipped(next)).toBe(false);
    expect(outputClipTitle(next)).toBe("");
  });

  it("latches an over of the sum and keeps it after the level falls", () => {
    let l = latchOutputs(NO_CLIP, [out(1, 1.8, "Main/Monitor", -2.1), out(2, 1.2, "Main/Monitor", -1.5)]);
    l = latchOutputs(l, [out(1, -6), out(2, -6)]);
    expect(clipped(l)).toBe(true);
    expect(l.channels).toEqual([1, 2]);
    expect(l.worstDb).toBeCloseTo(1.8);
    expect(l.reductionDb).toBeCloseTo(-2.1);
    const title = outputClipTitle(l);
    expect(title).toContain("Outputs 1/2 clipping (+1.8 dBFS)");
    expect(title).toContain("lower Main or the click");
    expect(title).toContain("2.1 dB");
  });

  it("names the bus to turn down on separate outputs", () => {
    const l = latchOutputs(NO_CLIP, [out(1, -3, "Main"), out(3, 0.4, "Monitor")]);
    expect(l.channels).toEqual([3]);
    expect(outputClipTitle(l)).toContain("Output 3 clipping (+0.4 dBFS) – lower Monitor or the click");
  });

  it("remembers the limiter even when nothing reached full scale", () => {
    const l = latchOutputs(NO_CLIP, [out(1, -0.2, "Main", -0.4)]);
    expect(clipped(l)).toBe(false);
    expect(l.reductionDb).toBeCloseTo(-0.4);
  });
});
