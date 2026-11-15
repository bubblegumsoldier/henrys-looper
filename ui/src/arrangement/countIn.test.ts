import { describe, expect, it } from "vitest";
import { DEFAULT_COUNT_IN, clampCountIn, docCountIn, nextCountIn, withoutCountIn } from "./countInValue";
import * as ops from "./ops";
import { henry } from "./fixtures";

describe("count-in", () => {
  it("is two bars when the score does not say, and the score's value when it does", () => {
    const doc = henry();
    delete doc.count_in;
    expect(DEFAULT_COUNT_IN).toBe(2);
    expect(docCountIn(doc)).toBe(2);
    expect(docCountIn(ops.setCountIn(doc, 3))).toBe(3);
    expect(docCountIn(null)).toBe(2);
  });

  it("is written into the document within 1..8 and leaves the original alone", () => {
    const doc = henry();
    expect(ops.setCountIn(doc, 0).count_in).toBe(1);
    expect(ops.setCountIn(doc, 12).count_in).toBe(8);
    expect(ops.setCountIn(doc, 2.4).count_in).toBe(2);
    expect(doc.count_in).toBeUndefined();
    expect(clampCountIn(Number.NaN)).toBe(1);
  });

  it("the live button cycles 1, 2, 3, 4", () => {
    expect([1, 2, 3, 4, 8].map(nextCountIn)).toEqual([2, 3, 4, 1, 1]);
  });

  it("a changed count_in line alone does not make the loaded score stale", () => {
    const a = "title: x\nbpm: 120\ncount_in: 2  # two bars\ntracks: {}\n";
    const b = "title: x\nbpm: 120\ncount_in: 4\ntracks: {}\n";
    expect(withoutCountIn(a)).toBe(withoutCountIn(b));
    expect(withoutCountIn(a)).toBe("title: x\nbpm: 120\ntracks: {}\n");
  });
});
