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
//!   written into it stayed below [`SILENT_PASS_PEAK`], its layer is dropped again. A first take is
//!   never judged - it defines the loop.
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

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    on: bool,
    /// The running take began while `on` and is checked for silence when it closes.
    judged: bool,
    /// Loudest absolute sample written into the running take.
    peak: f32,
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

    /// A take began on `track`. An overdub that begins while the latch is on is judged later.
    #[inline]
    pub fn begun(&mut self, track: usize, overdub: bool) {
        if let Some(s) = self.slots.get_mut(track) {
            s.judged = s.on && overdub;
            s.peak = 0.0;
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

    /// The running take closed. True when it was a judged pass in which nothing was played - its
    /// layer is to be dropped.
    #[inline]
    pub fn closed_silent(&mut self, track: usize) -> bool {
        let Some(s) = self.slots.get_mut(track) else {
            return false;
        };
        let silent = s.judged && s.peak < SILENT_PASS_PEAK;
        s.judged = false;
        s.peak = 0.0;
        silent
    }
}
