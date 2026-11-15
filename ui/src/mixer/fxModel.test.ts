import { describe, expect, it } from "vitest";
import { fxOn, fxOnList, presetLabel, shownPreset } from "./fxModel";
import type { FxPresetName, FxSlotName } from "../types";

const chain = (preset: FxPresetName, on: FxSlotName[]) => ({
  preset,
  effects: (["high_pass", "eq", "comp", "delay", "reverb"] as FxSlotName[]).map((name) => ({
    name,
    label: name,
    on: on.includes(name),
  })),
});

describe("effect chain model", () => {
  it("lists the effects that are on, in signal order", () => {
    const fx = chain("custom", ["reverb", "comp"]);
    expect(fxOnList(fx)).toEqual(["comp", "reverb"]);
    expect(fxOn(fx, "reverb")).toBe(true);
    expect(fxOn(fx, "delay")).toBe(false);
  });

  it("never shows Dry while an effect is on", () => {
    expect(presetLabel(shownPreset(chain("dry", [])))).toBe("Dry");
    expect(shownPreset(chain("dry", ["reverb"]))).toBe("custom");
    expect(presetLabel(shownPreset(chain("dry", ["reverb"])))).toBe("Custom");
  });

  it("keeps a preset's name and reports a hand-edited chain as Custom", () => {
    expect(presetLabel(shownPreset(chain("voice", ["high_pass", "eq", "comp", "reverb"])))).toBe("Voice");
    expect(presetLabel(shownPreset(chain("custom", ["reverb"])))).toBe("Custom");
  });
});
