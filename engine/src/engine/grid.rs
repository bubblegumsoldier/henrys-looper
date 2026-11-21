//! The loop geometry of one track, independent of what its layers hold.
//!
//! Every layer of a track - audio samples or MIDI notes - is addressed on one grid: `origin` (the
//! musical position loop index 0 corresponds to) and `loop_len` (in frames). The take that is being
//! recorded, the take queued behind it, and the rules for when either of them ends are the same for
//! both kinds of content, so they live here, in a plain `Copy` struct that knows nothing about
//! buffers. [`super::track::Track`] owns one and keeps the layers next to it.
//!
//! Every number in here counts **frames** on the musical axis, exactly as before the split.

/// Open recording window of one track, in musical coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Take {
    /// First musical sample that goes into the layer.
    pub start: u64,
    /// One past the last musical sample, once a stop has been received.
    pub end: Option<u64>,
    /// Index of the layer this take writes into.
    pub layer: usize,
    /// A take that defines the loop geometry (the first layer of a track) writes sequentially from
    /// index 0; an overdub writes into the existing grid and wraps at the loop end.
    pub defines_loop: bool,
}

/// The geometry of a take that is already scheduled while the previous one is still finishing.
/// Its storage (buffer or clip) is kept by the track beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingGeom {
    pub start: u64,
    pub end: Option<u64>,
    pub defines_loop: bool,
}

/// Origin, loop length and take state of one track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopGrid {
    /// Musical position that loop index 0 corresponds to.
    pub origin: u64,
    /// Loop length in **frames**; 0 means "no loop yet".
    pub loop_len: u64,
    /// Frames covered contiguously by a loop-defining take. Only that take needs it: it is what
    /// bounds the loop length when the take is closed.
    pub filled: u64,
    pub take: Option<Take>,
    pub pending: Option<PendingGeom>,
    /// A re-anchoring that has been armed: from this musical position on, loop index 0 is this
    /// position. Kept beside `origin` instead of overwriting it, because the two axes the grid is
    /// used on reach the position at different times - playback reads ahead of the output (plugin
    /// and chain latency), recording trails it by the roundtrip. Every position is therefore
    /// mapped through [`LoopGrid::origin_at`], and a take that ends on the very sample of the
    /// re-anchoring keeps writing its last frames on the old grid. Folded into `origin` when the
    /// next re-anchoring is armed; dropped with the loop.
    pub restart: Option<u64>,
}

impl LoopGrid {
    /// Forget the loop: the next take defines a new one.
    #[inline]
    pub fn reset(&mut self) {
        self.origin = 0;
        self.loop_len = 0;
        self.filled = 0;
        self.restart = None;
    }

    /// The origin that holds at musical position `pos`: the re-anchored one from its position on,
    /// the old one before.
    #[inline(always)]
    pub fn origin_at(&self, pos: u64) -> u64 {
        match self.restart {
            Some(at) if pos >= at => at,
            _ => self.origin,
        }
    }

    /// The origin of the newest geometry, armed or not - what a take that begins now and a display
    /// have to go by.
    #[inline]
    pub fn latest_origin(&self) -> u64 {
        self.restart.unwrap_or(self.origin)
    }

    /// Re-anchor the loop at `at`: from there on the loop plays from its top. A re-anchoring armed
    /// before is folded in first - it lies a section or more in the past by then.
    #[inline]
    pub fn arm_restart(&mut self, at: u64) {
        if let Some(previous) = self.restart.take() {
            self.origin = previous;
        }
        self.restart = Some(at);
    }

    /// Open the first take of a new loop, writing into layer `layer`.
    #[inline]
    pub fn begin_loop(&mut self, start: u64, layer: usize) {
        self.origin = start;
        self.restart = None;
        self.loop_len = 0;
        self.filled = 0;
        self.take = Some(Take {
            start,
            end: None,
            layer,
            defines_loop: true,
        });
    }

    /// Open a further layer on the existing grid.
    #[inline]
    pub fn begin_overdub(&mut self, start: u64, layer: usize) {
        self.take = Some(Take {
            start,
            end: None,
            layer,
            defines_loop: false,
        });
    }

    /// Musical position at which the running take has to end at the latest. A loop-defining take
    /// is bounded by its storage (`capacity` frames), an overdub by one pass through the loop.
    #[inline]
    pub fn take_limit(&self, capacity: u64) -> Option<u64> {
        let take = self.take?;
        if take.defines_loop {
            Some(take.start + capacity)
        } else {
            Some(take.start + self.loop_len)
        }
    }

    /// Where a frame at musical position `pos` goes in the running take's layer, or `None` if it
    /// does not belong there. A loop-defining take advances `filled`.
    #[inline]
    pub fn write_index(&mut self, pos: u64, capacity: u64) -> Option<u64> {
        let take = self.take?;
        if take.defines_loop {
            let index = pos.wrapping_sub(self.origin);
            if index >= capacity {
                return None;
            }
            // A loop-defining take is written strictly sequentially, so this is the only value
            // `index` can have.
            debug_assert_eq!(index, self.filled, "Luecke im Loop-Puffer");
            self.filled = index + 1;
            Some(index)
        } else {
            if self.loop_len == 0 {
                return None;
            }
            let origin = self.origin_at(pos);
            Some(pos.checked_sub(origin)? % self.loop_len)
        }
    }

    /// Announce where the running take ends - or, if a take is queued behind it, where that one
    /// ends: the control thread always sends start and stop of a take as a pair, in that order.
    pub fn set_take_end(&mut self, end: u64, capacity: u64) {
        if let Some(pending) = self.pending.as_mut() {
            pending.end = Some(end.max(pending.start));
            return;
        }
        if let Some(take) = self.take.as_mut() {
            let end = end.max(take.start);
            take.end = Some(end);
            if take.defines_loop {
                self.loop_len = (end - take.start).min(capacity);
            }
        }
    }

    /// Close the take. `end` is the musical position just after its last sample. Returns it.
    pub fn finish_take(&mut self, end: u64) -> Option<Take> {
        let take = self.take.take()?;
        if take.defines_loop {
            self.loop_len = end.saturating_sub(self.origin).min(self.filled);
        }
        Some(take)
    }

    #[inline]
    pub fn take_is_finishing(&self) -> bool {
        self.take.map(|t| t.end.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_defining_take_sets_the_loop_length_and_an_overdub_wraps() {
        let mut g = LoopGrid::default();
        g.begin_loop(1_000, 0);
        for i in 0..40 {
            assert_eq!(g.write_index(1_000 + i, 100), Some(i));
        }
        g.set_take_end(1_040, 100);
        assert_eq!(g.loop_len, 40);
        g.finish_take(1_040);
        g.begin_overdub(1_000 + 3 * 40 + 5, 1);
        assert_eq!(g.write_index(1_000 + 3 * 40 + 5, 100), Some(5));
        assert_eq!(g.take_limit(100), Some(1_000 + 3 * 40 + 5 + 40));
    }

    #[test]
    fn a_restart_moves_the_top_from_its_position_on_and_only_there() {
        let mut g = LoopGrid::default();
        g.begin_loop(1_000, 0);
        for i in 0..40 {
            g.write_index(1_000 + i, 100);
        }
        g.set_take_end(1_040, 100);
        g.finish_take(1_040);
        // An overdub running up to the re-anchoring keeps its old grid to its last frame ...
        g.begin_overdub(1_080, 1);
        g.arm_restart(1_090);
        assert_eq!(g.write_index(1_089, 100), Some(9));
        // ... and what comes after it is counted from the new top.
        assert_eq!(g.write_index(1_090, 100), Some(0));
        assert_eq!(g.write_index(1_131, 100), Some(1));
        assert_eq!(g.origin_at(1_089), 1_000);
        assert_eq!(g.latest_origin(), 1_090);
        // The next one folds this one in.
        g.arm_restart(1_200);
        assert_eq!(g.origin, 1_090);
        assert_eq!(g.origin_at(1_199), 1_090);
        assert_eq!(g.origin_at(1_200), 1_200);
        g.reset();
        assert_eq!(g.restart, None);
    }
}
