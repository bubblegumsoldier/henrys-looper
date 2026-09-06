//! The loop buffer of one track.
//!
//! Mono `Vec<f32>` (the plan settles this: guitar and voice are mono, which halves memory and
//! CPU). The buffer is allocated once by the control thread and handed over; the audio thread
//! only ever writes into it, reads from it and swaps the whole thing for another one. It never
//! grows, never shrinks and is never zero-filled inside the callback.
//!
//! Phase 1 holds exactly one layer. The split between "buffer plus loop geometry" (here) and
//! "when to record and play" (in `process.rs`) is what will let phase 2 put several layers behind
//! one track without touching the scheduling code - but no layer machinery is built on spec here.

/// What the track is doing, for the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrackState {
    /// Nothing recorded.
    #[default]
    Empty,
    /// A recording is scheduled but its start has not been reached yet.
    Armed,
    /// Recording in progress.
    Recording,
    /// Content available, silent.
    Ready,
    /// Content available, playing.
    Playing,
}

impl TrackState {
    /// German label for the terminal display.
    pub fn label(self) -> &'static str {
        match self {
            TrackState::Empty => "leer",
            TrackState::Armed => "scharf",
            TrackState::Recording => "Aufnahme",
            TrackState::Ready => "bereit",
            TrackState::Playing => "Wiedergabe",
        }
    }
}

pub struct LoopTrack {
    /// Pre-allocated, its length is the maximum loop length in samples.
    buffer: Vec<f32>,
    /// Musical position that loop index 0 corresponds to (the position recording started at).
    origin: u64,
    /// Loop length in samples; 0 means "no content".
    loop_len: u64,
    /// Samples written contiguously from index 0 during the current take. Reading beyond this is
    /// silence, which is what makes the record-to-play transition seamless: the tail of the loop
    /// is still being written while the head is already playing.
    filled: u64,
}

impl LoopTrack {
    /// Takes ownership of a buffer that somebody else allocated - by construction there is no
    /// allocation on this path.
    pub fn from_buffer(buffer: Vec<f32>) -> Self {
        Self {
            buffer,
            origin: 0,
            loop_len: 0,
            filled: 0,
        }
    }

    /// Maximum loop length this track can hold, in samples.
    #[inline]
    pub fn capacity(&self) -> u64 {
        self.buffer.len() as u64
    }

    #[inline]
    pub fn loop_len(&self) -> u64 {
        self.loop_len
    }

    #[inline]
    pub fn filled(&self) -> u64 {
        self.filled
    }

    #[inline]
    pub fn has_content(&self) -> bool {
        self.loop_len > 0
    }

    /// Replace the buffer, returning the old one so the control thread can drop it.
    pub fn swap_buffer(&mut self, buffer: Vec<f32>) -> Vec<f32> {
        self.loop_len = 0;
        self.filled = 0;
        self.origin = 0;
        std::mem::replace(&mut self.buffer, buffer)
    }

    /// Start a new take at musical position `origin`.
    ///
    /// No memset: everything that can be read afterwards has to be written first, which `filled`
    /// enforces. Zeroing a multi-second buffer inside the callback would be exactly the kind of
    /// unbounded work that causes drop-outs.
    pub fn begin_take(&mut self, origin: u64) {
        self.origin = origin;
        self.loop_len = 0;
        self.filled = 0;
    }

    /// Announce how long the loop is going to be, before the last sample has been written.
    ///
    /// Needed because the write pointer trails the play pointer by the compensated latency: when
    /// the output reaches the end of the take, the last `R` samples are still on their way in.
    /// Publishing the length early is what lets playback start seamlessly at the loop boundary;
    /// `filled` keeps the not-yet-written tail silent instead of stale.
    pub fn set_length(&mut self, len: u64) {
        self.loop_len = len.min(self.capacity());
    }

    /// Close the take. `end` is the musical position just after the last sample of the loop.
    pub fn finish_take(&mut self, end: u64) {
        let len = end.saturating_sub(self.origin).min(self.filled);
        self.loop_len = len;
    }

    pub fn clear(&mut self) {
        self.loop_len = 0;
        self.filled = 0;
    }

    /// Write one sample of the running take. `pos` is the *musical* position of the sample, i.e.
    /// latency compensation has already been applied by the caller.
    #[inline]
    pub fn write(&mut self, pos: u64, sample: f32) {
        let idx = pos.wrapping_sub(self.origin);
        if idx >= self.capacity() {
            return;
        }
        // Takes are written strictly sequentially, so this is the only value `idx` can have.
        debug_assert_eq!(idx, self.filled, "Luecke im Loop-Puffer");
        self.buffer[idx as usize] = sample;
        self.filled = idx + 1;
    }

    /// Read the loop at musical position `pos`, wrapping around.
    ///
    /// Positions that were never written return silence instead of stale memory.
    #[inline]
    pub fn read(&self, pos: u64) -> f32 {
        if self.loop_len == 0 || pos < self.origin {
            return 0.0;
        }
        let idx = (pos - self.origin) % self.loop_len;
        if idx >= self.filled {
            return 0.0;
        }
        self.buffer[idx as usize]
    }

    /// Loop content, for tests and later WAV export.
    #[cfg(test)]
    pub fn content(&self) -> &[f32] {
        &self.buffer[..self.loop_len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_sequential_and_readable() {
        let mut t = LoopTrack::from_buffer(vec![0.0; 100]);
        t.begin_take(1000);
        for i in 0..40u64 {
            t.write(1000 + i, i as f32);
        }
        t.finish_take(1040);
        assert_eq!(t.loop_len(), 40);
        assert_eq!(t.read(1000), 0.0);
        assert_eq!(t.read(1039), 39.0);
        // Wrap-around: sample 40 of the loop is sample 0 again.
        assert_eq!(t.read(1040), 0.0);
        assert_eq!(t.read(1041), 1.0);
        assert_eq!(t.read(1000 + 40 * 7 + 13), 13.0);
    }

    #[test]
    fn unwritten_positions_are_silent() {
        let mut t = LoopTrack::from_buffer(vec![7.0; 100]);
        t.begin_take(0);
        for i in 0..10u64 {
            t.write(i, 1.0);
        }
        // Pretend the take was closed at 50 although only 10 samples arrived: the loop can only
        // be as long as what was actually written.
        t.finish_take(50);
        assert_eq!(t.loop_len(), 10);
        assert_eq!(t.read(9), 1.0);
        assert_eq!(t.read(10), 1.0); // wrapped, not the stale 7.0
    }

    #[test]
    fn writes_beyond_capacity_are_dropped() {
        let mut t = LoopTrack::from_buffer(vec![0.0; 8]);
        t.begin_take(0);
        for i in 0..20u64 {
            t.write(i, i as f32);
        }
        assert_eq!(t.filled(), 8);
    }

    #[test]
    fn clear_removes_content_but_keeps_memory() {
        let mut t = LoopTrack::from_buffer(vec![0.0; 64]);
        t.begin_take(0);
        t.write(0, 0.5);
        t.finish_take(1);
        assert!(t.has_content());
        t.clear();
        assert!(!t.has_content());
        assert_eq!(t.capacity(), 64);
        assert_eq!(t.read(0), 0.0);
    }
}
