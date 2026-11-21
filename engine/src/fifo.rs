//! The input FIFO between the input callback and the output callback (the app and the CLI
//! `live`). cpal has no duplex callback, so the input stream's frames reach the engine - which runs
//! in the output callback - through this lock-free queue.
//!
//! # What it promises
//!
//! * **Whole frames only**, on both sides: half a frame would shift every channel of every later
//!   recording against the others.
//! * **Nothing is dropped while there is room** - and there is room for [`FIFO_BLOCKS`] driver
//!   blocks (170 ms at 128 frames): the output callback would have to stand still that long before
//!   a single input frame is lost. A lost frame shifts every later take by one frame (the engine
//!   numbers input frames as it consumes them, `m = k - R`), so this is the one thing that must
//!   not happen.
//! * **The backlog does not stay.** The output callback normally takes one block of input per
//!   block of output. When an input callback comes late (or after the output callback in the same
//!   cycle) the output finds the FIFO short ("input late") and takes what is there; the missing
//!   frames arrive with the next input callback and from then on wait in the FIFO one block longer.
//!   Taking exactly one block per block afterwards - as the FIFO did until 2026-11-21 - keeps that
//!   extra wait for the rest of the session: every such event added up to one block of monitoring
//!   delay, and after 64 of them the FIFO would overflow and lose input. Now the output callback
//!   takes everything beyond [`CUSHION_BLOCKS`] blocks in addition to its block. The engine records
//!   all of it (an input block may be longer than its output block); the monitor hears the first
//!   `n` frames and skips the rest once - the same moment the late input already made it skip.
//!   One block stays as a cushion, so the next late input callback is absorbed without a gap.
//!
//! The latency compensation is untouched by all of this: it works on frame numbers, not on when a
//! frame was consumed (`engine::live`, "Why the input goes through a FIFO").
//!
//! # Real time
//!
//! Both ends are `rtrb` chunk copies: no allocation, no lock. The ring is made resident when it is
//! built (`crate::rt::prefault_ring`).

use rtrb::{Consumer, Producer, RingBuffer};

/// Capacity in driver blocks.
pub const FIFO_BLOCKS: usize = 64;
/// Blocks of input the output callback leaves waiting before it takes the excess as well.
pub const CUSHION_BLOCKS: usize = 1;

/// The input callback's end.
pub struct FifoWriter {
    tx: Producer<f32>,
    channels: usize,
}

/// The output callback's end.
pub struct FifoReader {
    rx: Consumer<f32>,
    channels: usize,
    cushion: usize,
}

/// What one [`FifoReader::pull`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pulled {
    /// Frames waiting when the call began.
    pub waiting: usize,
    /// Frames taken.
    pub frames: usize,
    /// Fewer than the block asked for were there: the input was late.
    pub short: bool,
}

/// A FIFO for `channels` interleaved channels and driver blocks of `block_frames`.
pub fn input_fifo(block_frames: usize, channels: usize) -> (FifoWriter, FifoReader) {
    let channels = channels.max(1);
    let block = block_frames.max(1);
    let (mut tx, rx) = RingBuffer::<f32>::new(block * FIFO_BLOCKS * channels);
    crate::rt::prefault_ring(&mut tx);
    (
        FifoWriter { tx, channels },
        FifoReader {
            rx,
            channels,
            cushion: block * CUSHION_BLOCKS,
        },
    )
}

impl FifoWriter {
    /// Queue the whole frames of `data` (interleaved). Returns how many frames did **not** fit -
    /// lost input, 0 unless the output callback stood still for [`FIFO_BLOCKS`] blocks.
    #[inline]
    pub fn push(&mut self, data: &[f32]) -> usize {
        let frames = data.len() / self.channels;
        let room = self.tx.slots() / self.channels;
        let n = frames.min(room);
        if n > 0
            && let Ok(mut chunk) = self.tx.write_chunk(n * self.channels)
        {
            let (a, b) = chunk.as_mut_slices();
            let split = a.len();
            a.copy_from_slice(&data[..split]);
            b.copy_from_slice(&data[split..split + b.len()]);
            chunk.commit_all();
        }
        frames - n
    }
}

impl FifoReader {
    /// Frames waiting.
    #[inline]
    pub fn waiting(&self) -> usize {
        self.rx.slots() / self.channels
    }

    /// Input for an output block of `n` frames, into `dst` (interleaved; its length bounds how much
    /// can be taken). Normally `n` frames; fewer when fewer are there; more when more than `n` plus
    /// the cushion are waiting - the backlog of a late input callback, taken now instead of being
    /// carried for the rest of the session.
    #[inline]
    pub fn pull(&mut self, dst: &mut [f32], n: usize) -> Pulled {
        let waiting = self.waiting();
        let room = dst.len() / self.channels;
        let excess = waiting.saturating_sub(n + self.cushion);
        let want = (n + excess).min(waiting).min(room);
        if want > 0
            && let Ok(chunk) = self.rx.read_chunk(want * self.channels)
        {
            let (a, b) = chunk.as_slices();
            dst[..a.len()].copy_from_slice(a);
            dst[a.len()..a.len() + b.len()].copy_from_slice(b);
            chunk.commit_all();
        }
        Pulled {
            waiting,
            frames: want,
            short: want < n,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_frames_in_order_and_nothing_lost_while_there_is_room() {
        let (mut w, mut r) = input_fifo(4, 2);
        let data: Vec<f32> = (0..16).map(|i| i as f32).collect();
        assert_eq!(w.push(&data), 0);
        let mut dst = vec![0.0; 64];
        let p = r.pull(&mut dst, 4);
        assert_eq!(p, Pulled { waiting: 8, frames: 4, short: false });
        assert_eq!(&dst[..8], &data[..8]);
        // 4 waiting = one block plus the cushion of one: nothing extra yet.
        let p = r.pull(&mut dst, 4);
        assert_eq!(p.frames, 4);
        assert_eq!(&dst[..8], &data[8..16]);
        let p = r.pull(&mut dst, 4);
        assert_eq!(p, Pulled { waiting: 0, frames: 0, short: true });
    }

    /// Input that comes three blocks late, then all at once: what is left waiting afterwards.
    fn after_a_late_input(cushion: Option<usize>) -> usize {
        let (mut w, mut r) = input_fifo(128, 1);
        if let Some(c) = cushion {
            r.cushion = c;
        }
        let block = vec![0.0; 128];
        let mut dst = vec![0.0; 128 * 16];
        for _ in 0..10 {
            w.push(&block);
            r.pull(&mut dst, 128);
        }
        // Three output callbacks find nothing; the three blocks and the current one come together.
        for _ in 0..3 {
            assert!(r.pull(&mut dst, 128).short);
        }
        for _ in 0..4 {
            w.push(&block);
        }
        for _ in 0..100 {
            r.pull(&mut dst, 128);
            w.push(&block);
        }
        r.waiting()
    }

    /// What the FIFO did before: one block per block, so a late input left its blocks waiting for
    /// good - three blocks (8 ms) of monitoring delay for the rest of the session, after one
    /// hiccup. Now one block stays, as the cushion that absorbs the next one.
    #[test]
    fn a_late_input_no_longer_stays_as_delay() {
        assert_eq!(after_a_late_input(Some(usize::MAX / 2)), 4 * 128, "before");
        assert_eq!(after_a_late_input(None), 2 * 128, "now: the block in flight plus the cushion");
    }

    #[test]
    fn a_full_fifo_loses_only_what_does_not_fit_and_counts_it() {
        let (mut w, _r) = input_fifo(2, 1);
        let lost = w.push(&vec![0.5; 2 * FIFO_BLOCKS + 3]);
        assert_eq!(lost, 3);
    }

    #[test]
    fn the_backlog_of_a_late_input_shrinks_to_the_cushion() {
        let (mut w, mut r) = input_fifo(128, 1);
        let block = vec![0.0; 128];
        let mut dst = vec![0.0; 128 * 16];
        // Five input blocks piled up behind a stalled output.
        for _ in 0..5 {
            w.push(&block);
        }
        let p = r.pull(&mut dst, 128);
        // Took its block and everything beyond one block of cushion.
        assert_eq!(p.frames, 4 * 128);
        assert_eq!(r.waiting(), 128);
        // From then on one block per block, the cushion stays.
        w.push(&block);
        assert_eq!(r.pull(&mut dst, 128).frames, 128);
        assert_eq!(r.waiting(), 128);
    }
}
