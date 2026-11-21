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
//!
//! # What a late callback leaves behind
//!
//! The numbers above say *that* a callback was late. To say *why*, every callback carries a
//! [`BlockProfile`]: the time of each phase of the engine's block (commands, instruments, the
//! track mix with its plugins, take writes, recorder taps, status, output stage), per track where
//! the block path renders track by track, which command kinds were applied, how many allocations
//! and frees happened on the audio thread (`crate::rt`), and the thread's own user and system CPU
//! time. When a callback is late - its own duration over budget, or it started late - the clock
//! pushes a [`LateDetail`] (that block, the one before it, and the timing between them) into a
//! small lock-free queue; the control thread turns it into the overrun's log line and marker. The
//! profile costs a dozen clock reads per block (~0.5 us) and two `thread_info` calls (~1 us).

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use crate::engine::command::{COMMAND_KINDS, MAX_TRACKS};

/// A callback start this many block durations after the previous one counts as a late wake-up.
pub const LATE_WAKEUP_FACTOR: u64 = 2;
/// Sub-windows per published window.
const SUBS: usize = 4;
/// Late callbacks the queue to the control thread holds; more in between two reads are dropped
/// (and still counted).
pub const DETAIL_QUEUE: usize = 32;

/// The phases of one audio callback, in the order they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Phase {
    /// Everything before the first frame is rendered: buffers and commands collected from the
    /// control thread, rack and recorder hand-overs, MIDI clips, the input FIFO.
    Prepare = 0,
    /// The instruments (plugin time included) and the MIDI events they are sent.
    Instruments,
    /// Applying the commands that fell due in this block.
    Commands,
    /// Loops, monitoring, effect chains, inserts, faders and panners of every track, the bus
    /// inserts, the click, and the recorder's stem and mix taps.
    Mix,
    /// Writing the input into the running takes, the live signal into the recorder, MIDI takes.
    Record,
    /// The recorder handing finished frames to its writer thread.
    Capture,
    /// Meters and the status snapshot for the control thread.
    Status,
    /// Placing the buses on the device's channels, output meters, the safety limiter.
    Output,
}

pub const PHASES: usize = 8;
pub const PHASE_NAMES: [&str; PHASES] = [
    "prepare",
    "instruments",
    "commands",
    "mix",
    "record",
    "capture",
    "status",
    "output",
];

/// Where one callback's time went. Filled by `EngineCore` (phases, tracks, commands) and the
/// host's callback (allocations, CPU time, FIFO). Plain numbers, `Copy`, no allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockProfile {
    /// Wall time per [`Phase`], ns.
    pub phase_ns: [u32; PHASES],
    /// Wall time per track in the block path (taken while plugin inserts are installed - the path
    /// that renders track by track, plugins included). Zero in the frame path.
    pub track_ns: [u32; MAX_TRACKS],
    /// Time inside plugins (instruments and inserts), ns - part of the phases above.
    pub plugin_ns: u32,
    /// Bit `k` set: a command of kind `k` (`Command::kind`) was applied in this callback.
    pub command_mask: u64,
    /// Commands applied in this callback.
    pub commands: u16,
    /// Allocations and frees on the audio thread during this callback (`crate::rt`).
    pub allocs: u32,
    pub frees: u32,
    /// The callback thread's CPU time during this callback, ns; `cpu_known` false where the
    /// platform does not tell.
    pub user_ns: u32,
    pub system_ns: u32,
    pub cpu_known: bool,
    /// Input frames waiting in the host's FIFO when the callback began, and taken by it.
    pub fifo_frames: u32,
    pub input_frames: u32,
}

impl BlockProfile {
    #[inline]
    pub fn add_phase(&mut self, phase: Phase, ns: u64) {
        let slot = &mut self.phase_ns[phase as usize];
        *slot = slot.saturating_add(ns.min(u64::from(u32::MAX)) as u32);
    }

    #[inline]
    pub fn add_track(&mut self, track: usize, ns: u64) {
        if let Some(slot) = self.track_ns.get_mut(track) {
            *slot = slot.saturating_add(ns.min(u64::from(u32::MAX)) as u32);
        }
    }

    #[inline]
    pub fn note_command(&mut self, kind: usize) {
        self.command_mask |= 1u64 << (kind as u64 & 63);
        self.commands = self.commands.saturating_add(1);
    }

    /// Names of the command kinds applied, in kind order.
    pub fn command_names(&self) -> Vec<&'static str> {
        COMMAND_KINDS
            .iter()
            .enumerate()
            .filter(|(k, _)| self.command_mask & (1u64 << k) != 0)
            .map(|(_, name)| *name)
            .collect()
    }

    /// The phases that took longest, slowest first, with their ns - for a log line.
    pub fn slowest_phases(&self, n: usize) -> Vec<(&'static str, u32)> {
        let mut v: Vec<(&'static str, u32)> = PHASE_NAMES
            .iter()
            .copied()
            .zip(self.phase_ns.iter().copied())
            .filter(|(_, ns)| *ns > 0)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.truncate(n);
        v
    }

    /// Wall time not accounted for by user and system CPU time: the thread was not running
    /// (preempted, or waiting on a lock or on I/O such as a page-in). `None` without CPU times.
    pub fn off_cpu_ns(&self, busy_ns: u64) -> Option<u64> {
        self.cpu_known
            .then(|| busy_ns.saturating_sub(u64::from(self.user_ns) + u64::from(self.system_ns)))
    }
}

/// Which way a callback was late.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lateness {
    /// Its own processing took longer than the block's budget.
    Block,
    /// It started more than [`LATE_WAKEUP_FACTOR`] blocks after the previous callback started.
    WakeUp,
    /// Both.
    Both,
}

/// One late callback, as the control thread gets it: the block itself, the one before it, and
/// the timing between them. All durations in ns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LateDetail {
    pub lateness: Lateness,
    /// Engine frame at the start of the late callback.
    pub frame: u64,
    pub frames: u32,
    /// The block's budget (its playing time).
    pub budget_ns: u64,
    /// The late callback's own duration.
    pub busy_ns: u64,
    /// From the previous callback's start to this one's - normally one budget.
    pub interval_ns: u64,
    /// From the previous callback's end to this one's start: how long the thread waited for its
    /// wake-up.
    pub idle_ns: u64,
    /// The previous callback's duration.
    pub prev_busy_ns: u64,
    pub profile: BlockProfile,
    pub prev_profile: BlockProfile,
}


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
    /// Allocations and frees on the audio thread since the stream started (`crate::rt`). Must stay
    /// zero; the control thread logs when they move.
    rt_allocs: AtomicU64,
    rt_frees: AtomicU64,
    /// Late callbacks whose detail did not fit into the queue.
    details_dropped: AtomicU64,
    /// The callback thread's scheduling priority and policy as last seen (`crate::rt`), 0 unknown.
    priority: AtomicI32,
    policy: AtomicI32,
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
    pub rt_allocs: u64,
    pub rt_frees: u64,
    pub details_dropped: u64,
    pub priority: i32,
    pub policy: i32,
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
            rt_allocs: self.rt_allocs.load(Ordering::Relaxed),
            rt_frees: self.rt_frees.load(Ordering::Relaxed),
            details_dropped: self.details_dropped.load(Ordering::Relaxed),
            priority: self.priority.load(Ordering::Relaxed),
            policy: self.policy.load(Ordering::Relaxed),
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
    /// Start and end of the previous callback (ns since `origin`), and its block duration.
    prev: Option<(u64, u64, u64)>,
    prev_profile: BlockProfile,
    current: Sub,
    current_frames: u64,
    sub_frames: u64,
    done: [Sub; SUBS],
    next: usize,
    details: rtrb::Producer<LateDetail>,
    details_rx: Option<rtrb::Consumer<LateDetail>>,
}

impl LoadClock {
    /// A clock for a stream at `sample_rate`, and the handle the control thread reads. The queue
    /// of late callbacks is fetched once with [`LoadClock::take_details`].
    pub fn new(sample_rate: u32) -> (Self, Arc<LoadShared>) {
        let shared = Arc::new(LoadShared::new());
        let rate = u64::from(sample_rate.max(1));
        let (mut details, details_rx) = rtrb::RingBuffer::new(DETAIL_QUEUE);
        crate::rt::prefault_ring(&mut details);
        let clock = Self {
            shared: Arc::clone(&shared),
            rate,
            origin: Instant::now(),
            frames: 0,
            prev: None,
            prev_profile: BlockProfile::default(),
            current: Sub::default(),
            current_frames: 0,
            sub_frames: (rate / SUBS as u64).max(1),
            done: [Sub::default(); SUBS],
            next: 0,
            details,
            details_rx: Some(details_rx),
        };
        (clock, shared)
    }

    /// The control thread's end of the late-callback queue. `Some` once.
    pub fn take_details(&mut self) -> Option<rtrb::Consumer<LateDetail>> {
        self.details_rx.take()
    }

    /// End of a callback that began at `started` (read at the top of the callback) and produced
    /// `frames` frames, `plugin_ns` of which were spent inside plugins.
    #[inline]
    pub fn finish(&mut self, started: Instant, frames: usize, plugin_ns: u64) {
        let profile = BlockProfile {
            plugin_ns: plugin_ns.min(u64::from(u32::MAX)) as u32,
            ..BlockProfile::default()
        };
        self.finish_profiled(started, frames, &profile);
    }

    /// The same with the block's profile - what the app's callback hands in.
    #[inline]
    pub fn finish_profiled(&mut self, started: Instant, frames: usize, profile: &BlockProfile) {
        let start_ns = started.saturating_duration_since(self.origin).as_nanos() as u64;
        let busy_ns = started.elapsed().as_nanos() as u64;
        self.record_profiled(start_ns, busy_ns, frames, profile);
    }

    /// Note the callback thread's scheduling, as `crate::rt::thread_times` reported it.
    #[inline]
    pub fn note_thread(&self, priority: i32, policy: i32) {
        self.shared.priority.store(priority, Ordering::Relaxed);
        self.shared.policy.store(policy, Ordering::Relaxed);
    }

    /// The arithmetic of [`finish`](Self::finish), on plain numbers - what the tests drive.
    pub fn record(&mut self, start_ns: u64, busy_ns: u64, frames: usize, plugin_ns: u64) {
        let profile = BlockProfile {
            plugin_ns: plugin_ns.min(u64::from(u32::MAX)) as u32,
            ..BlockProfile::default()
        };
        self.record_profiled(start_ns, busy_ns, frames, &profile);
    }

    /// [`record`](Self::record) with a full profile.
    pub fn record_profiled(
        &mut self,
        start_ns: u64,
        busy_ns: u64,
        frames: usize,
        profile: &BlockProfile,
    ) {
        let frames = frames as u64;
        if frames == 0 {
            return;
        }
        let plugin_ns = u64::from(profile.plugin_ns);
        let s = &*self.shared;
        let budget_ns = frames * 1_000_000_000 / self.rate;
        let permille = permille(busy_ns, budget_ns);
        s.callbacks.fetch_add(1, Ordering::Relaxed);
        s.block_frames.store(frames as u32, Ordering::Relaxed);
        if profile.allocs > 0 {
            s.rt_allocs
                .fetch_add(u64::from(profile.allocs), Ordering::Relaxed);
        }
        if profile.frees > 0 {
            s.rt_frees.fetch_add(u64::from(profile.frees), Ordering::Relaxed);
        }

        let late_block = busy_ns > budget_ns;
        let late_wakeup = self.prev.is_some_and(|(prev_start, _, prev_budget)| {
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
            let (interval_ns, idle_ns, prev_busy_ns) = match self.prev {
                Some((prev_start, prev_end, _)) => (
                    start_ns.saturating_sub(prev_start),
                    start_ns.saturating_sub(prev_end),
                    prev_end.saturating_sub(prev_start),
                ),
                None => (0, 0, 0),
            };
            let detail = LateDetail {
                lateness: match (late_block, late_wakeup) {
                    (true, true) => Lateness::Both,
                    (true, false) => Lateness::Block,
                    _ => Lateness::WakeUp,
                },
                frame: self.frames,
                frames: frames as u32,
                budget_ns,
                busy_ns,
                interval_ns,
                idle_ns,
                prev_busy_ns,
                profile: *profile,
                prev_profile: self.prev_profile,
            };
            if self.details.push(detail).is_err() {
                s.details_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.prev = Some((start_ns, start_ns + busy_ns, budget_ns));
        self.prev_profile = *profile;
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

    /// A late block hands over its own record; the callback after it, which starts late because
    /// of it, hands over a second one that carries the slow block's profile as "the one before".
    /// Callbacks on time hand over nothing. Allocations add up in the shared counters.
    #[test]
    fn late_callbacks_leave_their_records_and_on_time_ones_none() {
        let (mut clock, shared) = LoadClock::new(RATE);
        let mut details = clock.take_details().unwrap();
        assert!(clock.take_details().is_none(), "one consumer");
        let mut t = 0;
        run(&mut clock, &mut t, 10, BUDGET / 10, 0);
        assert!(details.pop().is_err());
        let mut slow = BlockProfile::default();
        slow.add_phase(Phase::Commands, 3_000_000);
        slow.note_command(7);
        slow.allocs = 2;
        slow.frees = 1;
        // 6 ms: the next callback cannot start before more than two budgets have passed.
        clock.record_profiled(t, 6_000_000, BLOCK, &slow);
        t += 6_100_000;
        clock.record(t, BUDGET / 10, BLOCK, 0);
        let a = details.pop().unwrap();
        assert_eq!(a.lateness, Lateness::Block);
        assert_eq!(a.frame, 10 * BLOCK as u64);
        assert_eq!(a.busy_ns, 6_000_000);
        assert_eq!(a.profile.command_names(), vec!["CutTake"]);
        assert_eq!(a.profile.slowest_phases(1), vec![("commands", 3_000_000)]);
        let b = details.pop().unwrap();
        assert_eq!(b.lateness, Lateness::WakeUp);
        assert_eq!(b.interval_ns, 6_100_000);
        assert_eq!(b.prev_busy_ns, 6_000_000);
        assert_eq!(b.idle_ns, 100_000);
        assert_eq!(b.prev_profile, slow);
        assert!(details.pop().is_err());
        let r = shared.read();
        assert_eq!((r.rt_allocs, r.rt_frees), (2, 1));
    }

    #[test]
    fn off_cpu_time_is_what_the_cpu_times_do_not_cover() {
        let p = BlockProfile {
            user_ns: 300_000,
            system_ns: 200_000,
            cpu_known: true,
            ..Default::default()
        };
        assert_eq!(p.off_cpu_ns(4_000_000), Some(3_500_000));
        assert_eq!(BlockProfile::default().off_cpu_ns(4_000_000), None);
    }
}
