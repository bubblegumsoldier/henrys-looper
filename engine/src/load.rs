//! Audio load: how much of each block's time budget the output callback used, and when it did not
//! make it.
//!
//! The budget of a callback is the time its block takes to play: `frames / sample_rate`. A
//! callback that needs longer than that is late - the driver wanted the block before it was
//! finished, and on CoreAudio that is a dropout (a click) unless the safety offset happens to
//! cover it. Three things are counted, because they have three different causes:
//!
//! * **Late blocks** - processing time above the budget. Our own work (or a plugin's) was too slow.
//! * **Late wake-ups** - the interval between two callback *starts* was more than
//!   [`LATE_WAKEUP_FACTOR`] times the block duration. The callback itself may have been quick; the
//!   system woke it late (another process, a page fault, the machine was busy).
//! * **Driver overloads** - what CoreAudio itself reports. cpal 0.18 registers a listener for
//!   `kAudioDeviceProcessorOverload` on every stream and forwards it as `ErrorKind::Xrun`, so the
//!   existing `xruns` counter of the host already is this number; it is not duplicated here.
//!
//! The load itself is published as two numbers over the last second: the **average** (busy time /
//! budget, summed over the window) and the **peak** (the worst single callback). The peak is the
//! one that matters for dropouts; an average of 30 % with a peak of 105 % clicks.
//!
//! # Real time
//!
//! [`LoadClock`] lives inside the output callback and is owned by it. Per callback it reads the
//! monotonic clock once more (the start is read by the callback anyway), does a handful of integer
//! operations and, four times a second, stores a few atomics. No allocation, no lock, no syscall:
//! `Instant::now` is `mach_absolute_time` / `clock_gettime(CLOCK_MONOTONIC)` from the vDSO /
//! commpage. The control thread reads [`LoadShared`] with [`LoadShared::read`].
//!
//! The window is four sub-windows of a quarter second each, counted in **audio frames**, not in
//! wall time: the published values change four times a second and always cover the last second of
//! audio.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// A callback start this many block durations after the previous one counts as a late wake-up.
pub const LATE_WAKEUP_FACTOR: u64 = 2;
/// Sub-windows per published window.
const SUBS: usize = 4;

/// The values the callback publishes. Atomics only; read with [`LoadShared::read`].
#[derive(Default, Debug)]
pub struct LoadShared {
    /// Last second: busy / budget in tenths of a percent.
    avg_permille: AtomicU32,
    peak_permille: AtomicU32,
    /// Last second: time spent inside plugins (instruments and inserts) / budget.
    plugin_permille: AtomicU32,
    late_blocks: AtomicU64,
    late_wakeups: AtomicU64,
    callbacks: AtomicU64,
    /// Engine frame (output frames since the first callback) at the start of the last late block or
    /// late wake-up, and its load. `u64::MAX` while there was none.
    last_frame: AtomicU64,
    last_permille: AtomicU32,
    /// Frames of the newest callback - the buffer size the driver really uses.
    block_frames: AtomicU32,
}

/// One consistent-enough copy of [`LoadShared`] for the control thread. The fields are read one by
/// one, so two of them can belong to neighbouring windows - harmless for a display and a log.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoadReading {
    pub avg_percent: f32,
    pub peak_percent: f32,
    pub plugin_percent: f32,
    pub late_blocks: u64,
    pub late_wakeups: u64,
    pub callbacks: u64,
    /// Engine frame of the newest late block or late wake-up, `None` while there was none.
    pub last_frame: Option<u64>,
    /// Load of that callback in percent (for a late wake-up: of the callback that woke late).
    pub last_percent: f32,
    pub block_frames: u32,
}

impl LoadShared {
    pub fn new() -> Self {
        Self {
            last_frame: AtomicU64::new(u64::MAX),
            ..Self::default()
        }
    }

    pub fn read(&self) -> LoadReading {
        let last = self.last_frame.load(Ordering::Relaxed);
        LoadReading {
            avg_percent: self.avg_permille.load(Ordering::Relaxed) as f32 / 10.0,
            peak_percent: self.peak_permille.load(Ordering::Relaxed) as f32 / 10.0,
            plugin_percent: self.plugin_permille.load(Ordering::Relaxed) as f32 / 10.0,
            late_blocks: self.late_blocks.load(Ordering::Relaxed),
            late_wakeups: self.late_wakeups.load(Ordering::Relaxed),
            callbacks: self.callbacks.load(Ordering::Relaxed),
            last_frame: (last != u64::MAX).then_some(last),
            last_percent: self.last_permille.load(Ordering::Relaxed) as f32 / 10.0,
            block_frames: self.block_frames.load(Ordering::Relaxed),
        }
    }
}

/// Sums over one sub-window.
#[derive(Clone, Copy, Debug, Default)]
struct Sub {
    busy_ns: u64,
    budget_ns: u64,
    plugin_ns: u64,
    peak_permille: u32,
}

/// The callback's side. Owned by the output callback, never shared.
pub struct LoadClock {
    shared: Arc<LoadShared>,
    rate: u64,
    origin: Instant,
    /// Output frames processed so far - the engine position of the next callback's first frame.
    frames: u64,
    /// Start of the previous callback (ns since `origin`) and its block duration.
    prev: Option<(u64, u64)>,
    current: Sub,
    current_frames: u64,
    sub_frames: u64,
    done: [Sub; SUBS],
    next: usize,
}

impl LoadClock {
    /// A clock for a stream at `sample_rate`, and the handle the control thread reads.
    pub fn new(sample_rate: u32) -> (Self, Arc<LoadShared>) {
        let shared = Arc::new(LoadShared::new());
        let rate = u64::from(sample_rate.max(1));
        let clock = Self {
            shared: Arc::clone(&shared),
            rate,
            origin: Instant::now(),
            frames: 0,
            prev: None,
            current: Sub::default(),
            current_frames: 0,
            sub_frames: (rate / SUBS as u64).max(1),
            done: [Sub::default(); SUBS],
            next: 0,
        };
        (clock, shared)
    }

    /// End of a callback that began at `started` (read at the top of the callback) and produced
    /// `frames` frames, `plugin_ns` of which were spent inside plugins.
    #[inline]
    pub fn finish(&mut self, started: Instant, frames: usize, plugin_ns: u64) {
        let start_ns = started.saturating_duration_since(self.origin).as_nanos() as u64;
        let busy_ns = started.elapsed().as_nanos() as u64;
        self.record(start_ns, busy_ns, frames, plugin_ns);
    }

    /// The arithmetic of [`finish`](Self::finish), on plain numbers - what the tests drive.
    pub fn record(&mut self, start_ns: u64, busy_ns: u64, frames: usize, plugin_ns: u64) {
        let frames = frames as u64;
        if frames == 0 {
            return;
        }
        let s = &*self.shared;
        let budget_ns = frames * 1_000_000_000 / self.rate;
        let permille = permille(busy_ns, budget_ns);
        s.callbacks.fetch_add(1, Ordering::Relaxed);
        s.block_frames.store(frames as u32, Ordering::Relaxed);

        let late_block = busy_ns > budget_ns;
        let late_wakeup = self.prev.is_some_and(|(prev_start, prev_budget)| {
            start_ns.saturating_sub(prev_start) > prev_budget * LATE_WAKEUP_FACTOR
        });
        if late_block {
            s.late_blocks.fetch_add(1, Ordering::Relaxed);
        }
        if late_wakeup {
            s.late_wakeups.fetch_add(1, Ordering::Relaxed);
        }
        if late_block || late_wakeup {
            s.last_frame.store(self.frames, Ordering::Relaxed);
            s.last_permille.store(permille, Ordering::Relaxed);
        }
        self.prev = Some((start_ns, budget_ns));
        self.frames += frames;

        let c = &mut self.current;
        c.busy_ns += busy_ns;
        c.budget_ns += budget_ns;
        c.plugin_ns += plugin_ns;
        c.peak_permille = c.peak_permille.max(permille);
        self.current_frames += frames;
        if self.current_frames >= self.sub_frames {
            self.done[self.next] = std::mem::take(&mut self.current);
            self.next = (self.next + 1) % SUBS;
            self.current_frames = 0;
            self.publish();
        }
    }

    fn publish(&self) {
        let mut sum = Sub::default();
        for d in &self.done {
            sum.busy_ns += d.busy_ns;
            sum.budget_ns += d.budget_ns;
            sum.plugin_ns += d.plugin_ns;
            sum.peak_permille = sum.peak_permille.max(d.peak_permille);
        }
        let s = &*self.shared;
        s.avg_permille
            .store(permille(sum.busy_ns, sum.budget_ns), Ordering::Relaxed);
        s.peak_permille.store(sum.peak_permille, Ordering::Relaxed);
        s.plugin_permille
            .store(permille(sum.plugin_ns, sum.budget_ns), Ordering::Relaxed);
    }
}

#[inline]
fn permille(part: u64, whole: u64) -> u32 {
    if whole == 0 {
        return 0;
    }
    (part.saturating_mul(1000) / whole).min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// 128 frames at 48 kHz: 2 666 666 ns.
    const BLOCK: usize = 128;
    const BUDGET: u64 = 2_666_666;

    /// Feed `n` evenly spaced callbacks of `busy` ns each, starting at `*t`.
    fn run(clock: &mut LoadClock, t: &mut u64, n: usize, busy: u64, plugin: u64) {
        for _ in 0..n {
            clock.record(*t, busy, BLOCK, plugin);
            *t += BUDGET;
        }
    }

    #[test]
    fn half_the_budget_reads_as_fifty_percent_after_one_second() {
        let (mut clock, shared) = LoadClock::new(RATE);
        let mut t = 0;
        // Nothing is published before the first quarter second is full.
        run(&mut clock, &mut t, 10, BUDGET / 2, BUDGET / 4);
        assert_eq!(shared.read().avg_percent, 0.0);
        run(&mut clock, &mut t, 400, BUDGET / 2, BUDGET / 4);
        let r = shared.read();
        assert!((r.avg_percent - 50.0).abs() < 0.2, "{r:?}");
        assert!((r.peak_percent - 50.0).abs() < 0.2, "{r:?}");
        assert!((r.plugin_percent - 25.0).abs() < 0.5, "{r:?}");
        assert_eq!(r.late_blocks, 0);
        assert_eq!(r.late_wakeups, 0);
        assert_eq!(r.last_frame, None);
        assert_eq!(r.block_frames, 128);
        assert_eq!(r.callbacks, 410);
    }

    #[test]
    fn one_slow_callback_sets_the_peak_and_leaves_the_window_after_a_second() {
        let (mut clock, shared) = LoadClock::new(RATE);
        let mut t = 0;
        run(&mut clock, &mut t, 400, BUDGET / 5, 0);
        // One callback at 90 %: no overrun, but the peak shows it.
        run(&mut clock, &mut t, 1, BUDGET * 9 / 10, 0);
        run(&mut clock, &mut t, 100, BUDGET / 5, 0);
        let r = shared.read();
        assert!((r.peak_percent - 90.0).abs() < 0.2, "{r:?}");
        // The average hardly moves: one block in a second's worth.
        assert!(r.avg_percent < 21.0, "{r:?}");
        assert_eq!(r.late_blocks, 0);
        // A second and a bit later the window has forgotten it.
        run(&mut clock, &mut t, 400, BUDGET / 5, 0);
        assert!((shared.read().peak_percent - 20.0).abs() < 0.2);
    }

    #[test]
    fn a_callback_over_its_budget_is_a_late_block_with_its_frame_and_load() {
        let (mut clock, shared) = LoadClock::new(RATE);
        let mut t = 0;
        run(&mut clock, &mut t, 50, BUDGET / 4, 0);
        // 3.2 ms of 2.67: 120 %. It still starts on time.
        clock.record(t, BUDGET * 6 / 5, BLOCK, 0);
        t += BUDGET * 6 / 5;
        run(&mut clock, &mut t, 400, BUDGET / 4, 0);
        let r = shared.read();
        assert_eq!(r.late_blocks, 1);
        assert_eq!(r.late_wakeups, 0, "an interval of 1.2 blocks is no late wake-up");
        assert_eq!(r.last_frame, Some(50 * BLOCK as u64));
        assert!((r.last_percent - 120.0).abs() < 0.2, "{r:?}");
        assert!(r.peak_percent >= 119.9, "{r:?}");
    }

    #[test]
    fn a_gap_between_two_callbacks_is_a_late_wake_up_even_when_both_are_quick() {
        let (mut clock, shared) = LoadClock::new(RATE);
        let mut t = 0;
        run(&mut clock, &mut t, 20, BUDGET / 10, 0);
        // The system woke us 2.5 blocks after the previous start.
        t += BUDGET * 3 / 2;
        clock.record(t, BUDGET / 10, BLOCK, 0);
        let r = shared.read();
        assert_eq!(r.late_wakeups, 1);
        assert_eq!(r.late_blocks, 0);
        assert_eq!(r.last_frame, Some(20 * BLOCK as u64));
        // Exactly twice the block is still in time.
        t += 2 * BUDGET;
        clock.record(t, BUDGET / 10, BLOCK, 0);
        assert_eq!(shared.read().late_wakeups, 1);
    }

    #[test]
    fn the_budget_follows_the_block_size_of_each_callback() {
        let (mut clock, shared) = LoadClock::new(RATE);
        // 512 frames are 10.67 ms: 5 ms is under budget there, over budget for 128 frames.
        clock.record(0, 5_000_000, 512, 0);
        clock.record(10_666_666, 5_000_000, 128, 0);
        let r = shared.read();
        assert_eq!(r.late_blocks, 1);
        assert_eq!(r.block_frames, 128);
    }

    #[test]
    fn an_empty_callback_counts_nothing() {
        let (mut clock, shared) = LoadClock::new(RATE);
        clock.record(0, 1_000_000_000, 0, 0);
        assert_eq!(shared.read(), LoadShared::new().read());
    }
}
