//! Continuous overdub ("latch"): one press, and every following pass of the loop becomes a layer of
//! its own until the overdub is switched off.
//!
//! # Why in the engine and not in the control thread
//!
//! The control thread could, in principle, schedule the next one-pass overdub shortly before each
//! loop boundary. Three things speak against it:
//!
//! * **Seamlessness is free here.** A take ends in [`super::process::EngineCore`] when the *input*
//!   reaches its end - `R` frames after the output did. The sample that closes pass `n` is, in the
//!   same call, the first sample of pass `n + 1`. Nothing is scheduled, nothing can arrive late, no
//!   sample is lost or written twice.
//! * **Switching it off stays a plain flag.** A control thread that has already queued the next pass
//!   cannot take it back - the command queue has no "cancel".
//! * **An empty pass can only be judged here.** Whether anything was played is a property of the
//!   samples written into the take, and removing the layer has to happen *before* the next pass
//!   pushes its own (a `RemoveLayer` from outside is refused while a take runs).
//!
//! # The rules
//!
//! * While `on`, a take of this track that closes - by its own limit, or by a stop at the loop
//!   boundary - is followed at that very sample by an overdub take on the same grid. That includes
//!   the loop-defining first take: "record, then keep overdubbing" without a second press.
//! * A take that **began** while the latch was on is *judged* when it closes: if the loudest sample
//!   written into it stayed below [`SILENT_PASS_PEAK`] (a MIDI take: if no note was played), its
//!   layer is dropped again.
//! * A **loop-defining** take that began while the latch was on - the first pass of a score's
//!   `stack` section on an empty track - is judged as well, but it cannot simply go: it defines
//!   origin and length, and the grid must not move because the musician needed the first pass to
//!   get to his pads. Its layer stays as a muted **placeholder** for as long as it is the only
//!   one; the first pass that is kept replaces it (layer 0 goes, the new one becomes layer 1 on
//!   the very same grid). If the latch ends and the placeholder is still alone, the track is
//!   cleared - nothing was played, so nothing is there.
//! * MIDI: a pass whose only notes lie in its last pre-roll window (a hit a hair before the one of
//!   the next pass) counts as empty, and those notes are handed to the pass that begins there -
//!   they were meant for its downbeat. A pass with anything else in it keeps them, as before.
//! * Off: the running pass finishes where it would have, and nothing follows it. It is still judged.
//! * The layer ceiling ends the latch with [`super::command::Refusal::OverdubFull`]; a missing
//!   buffer with `NoBuffer`. Every stop, clear and transport change switches it off.
//!
//! Real-time cost: one comparison per recorded frame, a few stores per take boundary.

use super::command::MAX_TRACKS;
use super::frame::Frame;

/// Loudest sample a pass may have and still count as "nothing played": -40 dBFS.
///
/// Chosen between the two things it has to tell apart. The noise floor of an interface input with
/// the gain set for a guitar or a voice sits around -70 to -55 dBFS, hum and a breath near the mic
/// rarely past -45; a played note, even a soft one, peaks at -30 dBFS or louder. A peak rather than
/// an average, so one short accent in an otherwise empty pass keeps the layer - throwing away
/// something played is worse than keeping one silent layer too many.
pub const SILENT_PASS_PEAK: f32 = 0.01;

#[derive(Clone, Copy, Debug)]
struct Slot {
    on: bool,
    /// The running take began while `on` and is checked for silence when it closes.
    judged: bool,
    /// Loudest absolute sample written into the running take.
    peak: f32,
    /// Earliest musical position of a note-on in the running take; `u64::MAX` while there is none.
    first_note: u64,
    /// Layer 0 of this track is an empty, muted stand-in that only holds the loop's geometry.
    placeholder: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            on: false,
            judged: false,
            peak: 0.0,
            first_note: u64::MAX,
            placeholder: false,
        }
    }
}

/// What a judged pass amounted to when it closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Something was played (or the take was not judged): it stays.
    Kept,
    /// Nothing was played.
    Silent,
    /// MIDI: the only notes lie in the last pre-roll window - they belong to the next pass.
    EarlyOnly,
}

/// The latch state of every track. Fixed size, `Copy` slots, nothing allocated after construction.
#[derive(Clone, Copy, Debug, Default)]
pub struct Latches {
    slots: [Slot; MAX_TRACKS],
}

impl Latches {
    #[inline]
    pub fn on(&self, track: usize) -> bool {
        self.slots.get(track).is_some_and(|s| s.on)
    }

    #[inline]
    pub fn set(&mut self, track: usize, on: bool) {
        if let Some(s) = self.slots.get_mut(track) {
            s.on = on;
        }
    }

    pub fn all_off(&mut self) {
        for s in self.slots.iter_mut() {
            s.on = false;
        }
    }

    /// A take began on `track`. One that begins while the latch is on is judged when it closes -
    /// a loop-defining one included (see the module comment).
    #[inline]
    pub fn begun(&mut self, track: usize) {
        if let Some(s) = self.slots.get_mut(track) {
            s.judged = s.on;
            s.peak = 0.0;
            s.first_note = u64::MAX;
        }
    }

    /// A note-on at musical position `pos` went into the running MIDI take.
    #[inline]
    pub fn note_midi(&mut self, track: usize, pos: u64) {
        if let Some(s) = self.slots.get_mut(track)
            && pos < s.first_note
        {
            s.first_note = pos;
        }
    }

    #[inline]
    pub fn placeholder(&self, track: usize) -> bool {
        self.slots.get(track).is_some_and(|s| s.placeholder)
    }

    #[inline]
    pub fn set_placeholder(&mut self, track: usize, on: bool) {
        if let Some(s) = self.slots.get_mut(track) {
            s.placeholder = on;
        }
    }

    /// One frame went into the running take.
    #[inline]
    pub fn note(&mut self, track: usize, frame: Frame) {
        if let Some(s) = self.slots.get_mut(track) {
            let m = frame.l.abs().max(frame.r.abs());
            if m > s.peak {
                s.peak = m;
            }
        }
    }

    /// The running take closed at `end`. `preroll` is the MIDI pre-roll window in frames.
    #[inline]
    pub fn closed(&mut self, track: usize, end: u64, preroll: u64) -> Verdict {
        let Some(s) = self.slots.get_mut(track) else {
            return Verdict::Kept;
        };
        let verdict = if !s.judged || s.peak >= SILENT_PASS_PEAK {
            Verdict::Kept
        } else if s.first_note == u64::MAX {
            Verdict::Silent
        } else if s.first_note >= end.saturating_sub(preroll) {
            Verdict::EarlyOnly
        } else {
            Verdict::Kept
        };
        s.judged = false;
        s.peak = 0.0;
        s.first_note = u64::MAX;
        verdict
    }
}
