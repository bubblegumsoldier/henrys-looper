import { describe, expect, it } from "vitest";
import { compareDisk } from "./diskWatch";

describe("file changed on disk", () => {
  it("compares the file with the text last opened or saved here", () => {
    expect(compareDisk("a", "a", null)).toBe("same");
    expect(compareDisk("a", "b", null)).toBe("changed");
    // Unsaved typing here is not a change on disk.
    expect(compareDisk("a", "a", null)).toBe("same");
  });

  it("asks once per change: Keep mine silences that text, not the next one", () => {
    expect(compareDisk("a", "b", "b")).toBe("kept");
    expect(compareDisk("a", "c", "b")).toBe("changed");
  });

  it("has nothing to compare for an untitled score, and a vanished file is no change", () => {
    expect(compareDisk(null, "b", null)).toBe("same");
    expect(compareDisk("a", null, null)).toBe("gone");
  });
});
