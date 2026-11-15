//! Has the open score's file been changed on disk since it was opened or saved here?
//!
//! Asked when the window gets the focus back (the musician edited the file in another editor and
//! comes back) and before ⌘S writes over it. The answer is a bar in the arrangement with "Reload
//! from disk" and "Keep mine" - never a silent overwrite, never a silent reload. "Keep mine"
//! remembers the disk text it was shown, so the same change is not asked about twice; a further
//! change on disk is asked about again.

export type DiskState = "same" | "changed" | "kept" | "gone";

/**
 * `savedText`: the text as last opened or saved here (null for an untitled score). `disk`: the
 * file now (null when gone or unreadable). `kept`: the disk text "Keep mine" was chosen against.
 */
export function compareDisk(savedText: string | null, disk: string | null, kept: string | null): DiskState {
  if (savedText === null) return "same";
  if (disk === null) return "gone";
  if (disk === savedText) return "same";
  if (disk === kept) return "kept";
  return "changed";
}
