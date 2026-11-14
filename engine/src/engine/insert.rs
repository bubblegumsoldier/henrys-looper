//! Insert slots: foreign audio plugins (or anything else that processes a stereo block) in the
//! signal path of a track and of a bus.
//!
//! # Where they sit
//!
//! ```text
//!  Ebenen-Summe + Mithoeren ─► eingebaute Kette ─► Insert 1 … Insert 8 ─► Panorama ─► Bus
//!  Summe aller Tracks eines Busses ─► Bus-Insert 1 … 8 ─► (+ Klick auf Monitor) ─► Bus-Lautstaerke
//! ```
//!
//! **After the built-in chain, before the panner**, which is where a DAW channel strip has its
//! inserts: the built-in chain is the "channel EQ and dynamics" every track has, a plugin is the
//! extra colour on top, and the panner places the finished sound. The position is fixed rather than
//! freely movable between the built-in effects: the built-in chain is one block with its own
//! bypass, and splitting it to let a plugin sit between its EQ and its compressor would need the
//! chain to become a list of slots itself - not worth it while nobody has asked for it.
//!
//! **What is recorded stays dry**, exactly as for the built-in chain: [`super::process::EngineCore`]
//! records from the raw input slice and never sees an insert.
//!
//! **The click stays outside every insert.** A bus insert processes the sum of the tracks on that
//! bus; the click is added afterwards, so a limiter on the monitor bus does not pump with the
//! metronome and a reverb does not smear it.
//!
//! # One instance per slot, and what that means for split buses
//!
//! A track renders one signal per bus ([`super::track`]) because the loop and the monitored input
//! may go to different buses, and a nonlinear effect cannot be taken apart afterwards. The built-in
//! chain answers that with one chain per bus. A plugin cannot do the same cheaply - a second
//! instance doubles the CPU and the memory, needs its own editor window and its own state - so a
//! slot has **one** instance, and the rule is:
//!
//! | Routing of the track | What the inserts process |
//! |---|---|
//! | both buses on the same output (2-output interface) | the one mix - correct |
//! | both buses hear the same sources | the shared signal, copied to both - correct |
//! | loop and input on **different** buses (4+ outputs) | the **Main** signal only; Monitor gets the built-in chain without the plugins |
//!
//! The third case is the known limit; the status says when it applies (`inserts_main_only`).
//!
//! # Real time
//!
//! The audio thread never creates, loads, or drops a plugin. An [`Insert`] is built on the control
//! side, travels into the engine through a lock-free queue ([`InsertSender`] → [`InsertEndpoint`])
//! and goes back the same way when it is replaced or removed, to be dropped off the audio thread.
//! Bypass is an atomic flag in [`InsertShared`], so flipping it needs no message at all. Everything
//! the audio thread touches is allocated in [`InsertRack::new`], on the control thread.
//!
//! What this module **cannot** promise is that the plugin itself keeps the rules: a foreign plugin
//! may allocate, lock or crash inside its process call. The guarantees of this engine end at that
//! call - see `docs/architektur.md`, section 8.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use super::bus::{BUS_COUNT, Bus};
use super::command::MAX_TRACKS;

/// Insert slots per track and per bus. The design shows a handful; eight is more than a voice or a
/// guitar ever needs and keeps the rack a fixed array.
pub const MAX_INSERTS: usize = 8;

/// Largest block a plugin is ever handed, in frames. Longer device buffers are cut into pieces of
/// at most this size, so a plugin can be prepared once with a known maximum. 1024 frames is 21 ms
/// at 48 kHz - far above anything a looper is run with.
pub const MAX_BLOCK: usize = 1024;

/// How many messages the audio thread takes out of the queue per callback. Bounded work, and far
/// more than a human can produce between two callbacks.
const MESSAGES_PER_BLOCK: usize = 16;

/// Something that processes a stereo block in place: a hosted plugin, or a test double.
///
/// `process` is called on the audio thread with `left.len() == right.len() <= MAX_BLOCK`. It must
/// not allocate, lock or block - which the engine cannot enforce for foreign code, only ask for.
pub trait InsertProcessor: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
}

/// The part of an insert both sides see: switches the control thread flips and numbers the audio
/// thread reports. Atomics only.
#[derive(Debug, Default)]
pub struct InsertShared {
    bypass: AtomicBool,
    /// Latency the plugin reports, in frames. Used to pull a track's loop playback earlier by the
    /// same amount, so a plugin with lookahead does not put the loop behind the click.
    latency: AtomicU32,
    /// Blocks processed - the control side's proof that audio really goes through this slot.
    blocks: AtomicU64,
    /// Longest process call since the last read, in nanoseconds. Zero when nobody measured.
    peak_ns: AtomicU32,
    /// Peak of the output since the last read, as f32 bits.
    peak_out: AtomicU32,
}

impl InsertShared {
    pub fn new(latency: u32) -> Arc<Self> {
        let shared = Self::default();
        shared.latency.store(latency, Ordering::Relaxed);
        Arc::new(shared)
    }

    pub fn set_bypass(&self, on: bool) {
        self.bypass.store(on, Ordering::Relaxed);
    }

    pub fn bypass(&self) -> bool {
        self.bypass.load(Ordering::Relaxed)
    }

    pub fn set_latency(&self, frames: u32) {
        self.latency.store(frames, Ordering::Relaxed);
    }

    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    /// How many blocks went through this slot so far.
    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    /// Output peak since the last call, and reset.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak_out.swap(0, Ordering::Relaxed))
    }

    /// Longest process call since the last call, in microseconds, and reset.
    pub fn take_peak_us(&self) -> f32 {
        self.peak_ns.swap(0, Ordering::Relaxed) as f32 / 1000.0
    }

    #[inline]
    fn note_block(&self, peak: f32, ns: u32) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        // Load-compare-store instead of `fetch_max` on bits: the audio thread is the only writer,
        // and the reader only ever swaps in zero.
        let old = f32::from_bits(self.peak_out.load(Ordering::Relaxed));
        if peak > old {
            self.peak_out.store(peak.to_bits(), Ordering::Relaxed);
        }
        if ns > self.peak_ns.load(Ordering::Relaxed) {
            self.peak_ns.store(ns, Ordering::Relaxed);
        }
    }
}

/// One occupied slot: the processor that runs on the audio thread and the switches it shares.
pub struct Insert {
    pub processor: Box<dyn InsertProcessor>,
    pub shared: Arc<InsertShared>,
}

impl Insert {
    pub fn new(processor: Box<dyn InsertProcessor>, shared: Arc<InsertShared>) -> Self {
        Self { processor, shared }
    }
}

/// Which signal a slot belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InsertTarget {
    /// 0-based track index.
    Track(usize),
    Bus(Bus),
}

impl InsertTarget {
    /// The spelling the app and the MIDI addresses use: `track.1`, `bus.main`.
    pub fn address(self) -> String {
        match self {
            InsertTarget::Track(t) => format!("track.{}", t + 1),
            InsertTarget::Bus(b) => format!("bus.{}", b.name()),
        }
    }
}

/// A change to the rack, from the control thread to the audio thread.
pub enum InsertMsg {
    /// Put an insert into a slot. Whatever was there goes back.
    Install {
        target: InsertTarget,
        slot: usize,
        insert: Insert,
    },
    /// Empty a slot. Its insert goes back.
    Remove { target: InsertTarget, slot: usize },
    /// Move the insert in `from` to `to`, shifting the ones in between - the order of a chain.
    Move {
        target: InsertTarget,
        from: usize,
        to: usize,
    },
}

/// Control side of the rack's queues.
pub struct InsertSender {
    tx: Producer<InsertMsg>,
    retired: Consumer<Insert>,
}

impl InsertSender {
    pub fn send(&mut self, msg: InsertMsg) -> Result<(), String> {
        self.tx
            .push(msg)
            .map_err(|_| "The plugin queue is full. Try again in a moment.".to_string())
    }

    /// Take back everything the audio thread is done with. The processors are dropped here, on the
    /// calling thread - which is the point of the queue.
    pub fn drain_retired(&mut self) -> usize {
        let mut n = 0;
        while let Ok(insert) = self.retired.pop() {
            drop(insert);
            n += 1;
        }
        n
    }
}

/// Audio side of the rack's queues.
pub struct InsertEndpoint {
    rx: Consumer<InsertMsg>,
    retire: Producer<Insert>,
}

/// The queue pair between the control thread and the rack. `capacity` bounds both directions; a
/// retire queue that is full would force the audio thread to drop a plugin, so it is generous.
pub fn insert_channel(capacity: usize) -> (InsertSender, InsertEndpoint) {
    let (tx, rx) = RingBuffer::new(capacity);
    let (retire, retired) = RingBuffer::new(capacity * 2 + MAX_INSERTS * (MAX_TRACKS + BUS_COUNT));
    (InsertSender { tx, retired }, InsertEndpoint { rx, retire })
}

/// How a track's two bus signals relate in one segment - decides what the inserts process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spread {
    /// Both buses leave on the same output: one mix, in the Main slot; Monitor is silent.
    Collapsed,
    /// Both buses hear the same sources: the Main signal is the Monitor signal.
    Shared,
    /// The buses hear different sources: two different signals.
    Split,
}

type Slots = [Option<Insert>; MAX_INSERTS];

fn empty_slots() -> Slots {
    std::array::from_fn(|_| None)
}

/// A stereo scratch block, de-interleaved because that is what every plugin API wants.
pub struct Block {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl Block {
    fn new() -> Self {
        Self {
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
        }
    }

    #[inline]
    pub fn clear(&mut self, n: usize) {
        self.l[..n].fill(0.0);
        self.r[..n].fill(0.0);
    }
}

/// The audio thread's side: every slot of every track and bus, plus the scratch blocks the block
/// renderer needs. Everything here is allocated in [`InsertRack::new`].
pub struct InsertRack {
    endpoint: InsertEndpoint,
    tracks: Vec<Slots>,
    buses: [Slots; BUS_COUNT],
    /// Per track: the unpanned signal for each bus.
    pub(crate) track_scratch: Vec<[Block; BUS_COUNT]>,
    /// Per bus: the sum of the tracks, before bus inserts.
    pub(crate) bus_scratch: [Block; BUS_COUNT],
    /// Occupied slots, so the engine can keep its per-frame path while the rack is empty.
    installed: usize,
    /// Inserts the audio thread had to forget because the retire queue was full. Leaking is the
    /// lesser evil: dropping a plugin here could take milliseconds.
    leaked: u64,
}

impl InsertRack {
    pub fn new(endpoint: InsertEndpoint) -> Self {
        Self {
            endpoint,
            tracks: (0..MAX_TRACKS).map(|_| empty_slots()).collect(),
            buses: [empty_slots(), empty_slots()],
            track_scratch: (0..MAX_TRACKS).map(|_| [Block::new(), Block::new()]).collect(),
            bus_scratch: [Block::new(), Block::new()],
            installed: 0,
            leaked: 0,
        }
    }

    /// Whether anything is installed. With an empty rack the engine renders exactly as it did
    /// before inserts existed.
    #[inline]
    pub fn active(&self) -> bool {
        self.installed > 0
    }

    pub fn leaked(&self) -> u64 {
        self.leaked
    }

    /// Apply what the control thread sent. Bounded work per call.
    pub fn collect(&mut self) {
        for _ in 0..MESSAGES_PER_BLOCK {
            let Ok(msg) = self.endpoint.rx.pop() else {
                break;
            };
            match msg {
                InsertMsg::Install {
                    target,
                    slot,
                    insert,
                } => match self.slots_mut(target).and_then(|s| s.get_mut(slot)) {
                    Some(cell) => {
                        let old = cell.replace(insert);
                        if old.is_none() {
                            self.installed += 1;
                        }
                        if let Some(old) = old {
                            self.retire(old);
                        }
                    }
                    // A slot that does not exist: hand the insert straight back.
                    None => self.retire(insert),
                },
                InsertMsg::Remove { target, slot } => {
                    let old = self
                        .slots_mut(target)
                        .and_then(|s| s.get_mut(slot))
                        .and_then(Option::take);
                    if let Some(old) = old {
                        self.installed -= 1;
                        self.retire(old);
                    }
                }
                InsertMsg::Move { target, from, to } => {
                    if let Some(slots) = self.slots_mut(target)
                        && from < MAX_INSERTS
                        && to < MAX_INSERTS
                    {
                        // Rotating moves `Option<Insert>` values - pointers - and allocates nothing.
                        if from < to {
                            slots[from..=to].rotate_left(1);
                        } else if to < from {
                            slots[to..=from].rotate_right(1);
                        }
                    }
                }
            }
        }
    }

    fn slots_mut(&mut self, target: InsertTarget) -> Option<&mut Slots> {
        match target {
            InsertTarget::Track(t) => self.tracks.get_mut(t),
            InsertTarget::Bus(b) => Some(&mut self.buses[b.index()]),
        }
    }

    fn retire(&mut self, insert: Insert) {
        if let Err(rtrb::PushError::Full(insert)) = self.endpoint.retire.push(insert) {
            std::mem::forget(insert);
            self.leaked += 1;
        }
    }

    /// Sum of the latencies of the active inserts of one track, in frames.
    #[inline]
    pub fn track_latency(&self, track: usize) -> u64 {
        self.tracks
            .get(track)
            .map(|slots| {
                slots
                    .iter()
                    .flatten()
                    .filter(|i| !i.shared.bypass())
                    .map(|i| u64::from(i.shared.latency()))
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Whether a track has any insert that is not bypassed.
    #[inline]
    pub fn track_has_inserts(&self, track: usize) -> bool {
        self.tracks
            .get(track)
            .map(|s| s.iter().flatten().any(|i| !i.shared.bypass()))
            .unwrap_or(false)
    }

    /// Run a track's inserts over its scratch block for `bus`, the first `n` frames.
    #[inline]
    pub fn process_track(&mut self, track: usize, bus: usize, n: usize) {
        let (Some(slots), Some(blocks)) =
            (self.tracks.get_mut(track), self.track_scratch.get_mut(track))
        else {
            return;
        };
        let block = &mut blocks[bus];
        run_slots(slots, &mut block.l[..n], &mut block.r[..n]);
    }

    /// Run a bus's inserts over its scratch block, the first `n` frames.
    #[inline]
    pub fn process_bus(&mut self, bus: usize, n: usize) {
        let block = &mut self.bus_scratch[bus];
        run_slots(&mut self.buses[bus], &mut block.l[..n], &mut block.r[..n]);
    }
}

#[inline]
fn run_slots(slots: &mut Slots, l: &mut [f32], r: &mut [f32]) {
    for insert in slots.iter_mut().flatten() {
        if insert.shared.bypass() {
            continue;
        }
        let started = std::time::Instant::now();
        insert.processor.process(l, r);
        let ns = started.elapsed().as_nanos().min(u32::MAX as u128) as u32;
        // A plugin that hands back NaN or infinity would poison every bus it reaches and every
        // reverb behind it for good. Replace it with silence, here, where it entered.
        let mut peak = 0.0f32;
        for s in l.iter_mut().chain(r.iter_mut()) {
            if !s.is_finite() {
                *s = 0.0;
            }
            peak = peak.max(s.abs());
        }
        insert.shared.note_block(peak, ns);
    }
}

#[cfg(test)]
pub mod testing {
    //! Plain-Rust processors for the offline tests: no plugin, no device.

    use super::InsertProcessor;

    /// Multiplies by a constant.
    pub struct Gain(pub f32);

    impl InsertProcessor for Gain {
        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            for s in left.iter_mut().chain(right.iter_mut()) {
                *s *= self.0;
            }
        }
    }

    /// Delays both channels by a fixed number of frames - a plugin with lookahead.
    pub struct Delay {
        line: Vec<[f32; 2]>,
        pos: usize,
    }

    impl Delay {
        pub fn new(frames: usize) -> Self {
            Self {
                line: vec![[0.0; 2]; frames.max(1)],
                pos: 0,
            }
        }
    }

    impl InsertProcessor for Delay {
        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let out = self.line[self.pos];
                self.line[self.pos] = [*l, *r];
                self.pos = (self.pos + 1) % self.line.len();
                *l = out[0];
                *r = out[1];
            }
        }
    }

    /// Swaps left and right - proves the channels arrive and leave in the right order.
    pub struct Swap;

    impl InsertProcessor for Swap {
        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                std::mem::swap(l, r);
            }
        }
    }

    /// Writes NaN - a broken plugin.
    pub struct Broken;

    impl InsertProcessor for Broken {
        fn process(&mut self, left: &mut [f32], _right: &mut [f32]) {
            left.fill(f32::NAN);
        }
    }
}
