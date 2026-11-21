import { describe, expect, it } from "vitest";
import { readsDisk, reloadEverything, type ReloadSteps } from "./reload";

function steps(file: ReloadSteps["file"], disk: string | null, load: () => Promise<void> = async () => {}) {
  const log: string[] = [];
  const s: ReloadSteps = {
    file,
    peek: async (path) => {
      log.push(`peek ${path}`);
      return disk;
    },
    adopt: (text) => log.push(`adopt ${text}`),
    loadWithReset: async () => {
      log.push("load+reset");
      await load();
    },
  };
  return { s, log };
}

describe("Reload in the Arrangement", () => {
  it("reads the file from disk, puts it in the editor, then resets and loads - no question", async () => {
    const { s, log } = steps({ path: "/a.yaml", dirty: false, keptMine: false }, "new text");
    await reloadEverything(s);
    expect(log).toEqual(["peek /a.yaml", "adopt new text", "load+reset"]);
  });

  it("never throws away typing here that was not saved, nor a Keep mine", async () => {
    expect(readsDisk({ path: "/a.yaml", dirty: true, keptMine: false })).toBe(false);
    expect(readsDisk({ path: "/a.yaml", dirty: false, keptMine: true })).toBe(false);
    expect(readsDisk({ path: null, dirty: false, keptMine: false })).toBe(false);
    const { s, log } = steps({ path: "/a.yaml", dirty: true, keptMine: false }, "disk");
    await reloadEverything(s);
    expect(log).toEqual(["load+reset"]);
  });

  it("loads the editor's text when the file cannot be read", async () => {
    const { s, log } = steps({ path: "/gone.yaml", dirty: false, keptMine: false }, null);
    await reloadEverything(s);
    expect(log).toEqual(["peek /gone.yaml", "load+reset"]);
  });

  it("passes a broken file's errors on; nothing else happens after them", async () => {
    const { s, log } = steps({ path: "/a.yaml", dirty: false, keptMine: false }, "broken: [", async () => {
      throw new Error("line 1: unexpected end");
    });
    await expect(reloadEverything(s)).rejects.toThrow("unexpected end");
    expect(log).toEqual(["peek /a.yaml", "adopt broken: [", "load+reset"]);
  });
});
