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
//! **Anywhere in the track's chain, before the panner.** The built-in effects keep their fixed
//! order (octaver, high-pass, EQ, compressor, delay, reverb - presets and MIDI addresses depend on
//! it); every track insert carries an [`InsertPosition`]: the built-in effect it sits in front of,
//! or the end of the chain. The default - and what every rack file or score from before positions
//! says - is the end, after the reverb, where a DAW channel strip has its inserts. A cabinet IR
//! loader wants to sit in front of everything instead, on the dry signal:
//!
//! ```text
//!  Ebenen-Summe + Mithoeren ─► [IR] ─► Oktaver ─► Hochpass ─► [De-Esser] ─► EQ ─► … ─► Hall ─► [Plugins am Ende] ─► Panorama
//! ```
//!
//! The slot order is the plugins' order among themselves; the signal order is by position first
//! and slot second ([`InsertRack::process_track_at`]). The app keeps the slots sorted by position,
//! so slot order and signal order agree, and a MIDI address (`insert.<slot>`) does not change when
//! a plugin is moved across built-in effects only. How the built-in chain is split around plugins
//! is `fx::Chain::process_segment`; while no insert of a track sits before the end, the track
//! renders exactly as it did before positions existed.
//!
//! **Moving a plugin while audio runs** would put a step into the signal: the chain behind the
//! move point changes its input from one sample to the next. A reorder (`Move`, `Place`) of a
//! track that is heard through plugins is therefore applied under a short dip: the track's output
//! fades to zero over [`REORDER_FADE`] frames, the slots change while it is silent, and it fades
//! back in. Installing, removing and bus moves stay immediate, as before.
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
use super::fx::{FX_SLOTS, FxSlot};

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

/// Frames over which a track fades out before a reorder and in after it (about 5 ms at 48 kHz).
pub const REORDER_FADE: u32 = 256;

/// Where in a track's chain an insert sits: in front of built-in effect `n`
/// ([`FxSlot::index`]) or, with [`InsertPosition::END`], after the whole built-in chain. Buses have
/// no built-in chain; their inserts are always at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InsertPosition(u8);

impl InsertPosition {
    /// After the reverb - where every insert sat before positions existed.
    pub const END: InsertPosition = InsertPosition(FX_SLOTS as u8);

    /// In front of `slot`.
    pub fn before(slot: FxSlot) -> Self {
        InsertPosition(slot.index() as u8)
    }

    /// The built-in effect this sits in front of; `None` at the end.
    pub fn before_slot(self) -> Option<FxSlot> {
        FxSlot::all().get(usize::from(self.0)).copied()
    }

    /// `before: eq` as the score and the rack file write it; absent is the end. `None` for a name
    /// that is not a built-in effect.
    pub fn parse(before: Option<&str>) -> Option<Self> {
        match before {
            None => Some(Self::END),
            Some(name) => FxSlot::parse(name).map(Self::before),
        }
    }

    /// The name of the built-in effect in front of which this sits, `None` at the end.
    pub fn name(self) -> Option<&'static str> {
        self.before_slot().map(FxSlot::name)
    }

    /// 0 (before the octaver) to [`FX_SLOTS`] (the end).
    #[inline]
    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    /// From [`Self::index`]; anything past the chain is the end.
    pub fn from_index(index: usize) -> Self {
        InsertPosition(index.min(FX_SLOTS) as u8)
    }
}

impl Default for InsertPosition {
    fn default() -> Self {
        Self::END
    }
}

/// Something that processes a stereo block in place: a hosted plugin, or a test double.
///
/// `process` is called on the audio thread with `left.len() == right.len() <= MAX_BLOCK`. It must
/// not allocate, lock or block - which the engine cannot enforce for foreign code, only ask for.
pub trait InsertProcessor: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
    /// Where the musical clock stands at the first frame of the next [`Self::process`] call. Set
    /// by the rack right before every call; a processor that does not care ignores it.
    fn set_transport(&mut self, transport: &PluginTransport) {
        let _ = transport;
    }
}

/// What a hosted plugin is told about the musical clock: tempo, time signature, whether it runs,
/// and where it stands - all for the **first frame** of one block. A small `Copy` value the engine
/// computes once per block (per [`MAX_BLOCK`] chunk) and hands to every insert and instrument, so
/// the audio thread needs neither a lock nor an atomic to tell a plugin the time.
///
/// Positions count from bar 1 ([`super::timeline::Timeline::zero`]). While the transport stands
/// they are 0 and `playing` is false; the tempo stays valid. During a count-in they are negative:
/// bar 1 is the end of the count-in, so a plugin sees the pre-roll the way a DAW shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PluginTransport {
    pub sample_rate: f64,
    /// Quarter notes per minute.
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub playing: bool,
    /// Frames since bar 1.
    pub position: i64,
    /// Frames since the engine started, whatever the transport did - a clock that never jumps.
    pub continuous: u64,
}

impl PluginTransport {
    /// A transport that stands at bar 1 - what a plugin sees before the engine told it anything.
    pub fn standing(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            bpm: 120.0,
            beats_per_bar: 4,
            beat_unit: 4,
            playing: false,
            position: 0,
            continuous: 0,
        }
    }

    pub fn samples_per_quarter(&self) -> f64 {
        self.sample_rate * 60.0 / self.bpm
    }

    /// Position in quarter notes since bar 1 (VST3 `projectTimeMusic`, AU "current beat").
    pub fn quarters(&self) -> f64 {
        self.position as f64 / self.samples_per_quarter()
    }

    /// Length of one bar in quarter notes: 3 in 3/4, 3.5 in 7/8.
    pub fn bar_quarters(&self) -> f64 {
        self.beats_per_bar as f64 * 4.0 / self.beat_unit as f64
    }

    /// Start of the bar the position lies in, in quarter notes (VST3 `barPositionMusic`, AU
    /// "current measure downbeat"). Negative during a count-in.
    pub fn bar_start_quarters(&self) -> f64 {
        let bar = self.bar_quarters();
        // A hair of tolerance: a downbeat computed as `n * 2.5` divided back must not land in the
        // bar before it.
        ((self.quarters() + 1e-9) / bar).floor() * bar
    }

    /// The same transport `frames` later: the position only moves while it runs.
    #[must_use]
    pub fn advanced(mut self, frames: usize) -> Self {
        if self.playing {
            self.position += frames as i64;
        }
        self.continuous += frames as u64;
        self
    }
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
    /// Where in the track's chain it runs. Ignored on a bus.
    pub position: InsertPosition,
}

impl Insert {
    /// An insert at the end of the chain.
    pub fn new(processor: Box<dyn InsertProcessor>, shared: Arc<InsertShared>) -> Self {
        Self {
            processor,
            shared,
            position: InsertPosition::END,
        }
    }

    /// The same insert at `position` in the chain.
    #[must_use]
    pub fn at(mut self, position: InsertPosition) -> Self {
        self.position = position;
        self
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
    /// Move the insert in `from` to `to` (as `Move`) and put it at `position` in the chain.
    /// `from == to` changes only the position.
    Place {
        target: InsertTarget,
        from: usize,
        to: usize,
        position: InsertPosition,
    },
}

/// A reorder waiting for its track to have faded out.
#[derive(Clone, Copy, Debug)]
struct Reorder {
    from: usize,
    to: usize,
    position: Option<InsertPosition>,
}

/// The dip a track's output takes around a reorder: see the module comment.
#[derive(Clone, Copy, Debug)]
struct ReorderFade {
    /// Current gain, 0..=1.
    level: f32,
    /// Change per frame: negative while fading out, positive while fading in, 0 when idle.
    step: f32,
    pending: Option<Reorder>,
}

impl ReorderFade {
    const IDLE: Self = Self {
        level: 1.0,
        step: 0.0,
        pending: None,
    };
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
    /// The clock at the first frame of the block being processed; set by the engine per block.
    transport: PluginTransport,
    /// Per track: the dip around a reorder.
    fades: Vec<ReorderFade>,
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
            transport: PluginTransport::standing(48_000.0),
            fades: vec![ReorderFade::IDLE; MAX_TRACKS],
        }
    }

    /// The clock for the block that is processed next. Every insert gets it before its call.
    #[inline]
    pub fn set_transport(&mut self, transport: PluginTransport) {
        self.transport = transport;
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
    ///
    /// A reorder of a track waits for the track to fade out (see the module comment). While one
    /// waits, the queue is not read further: messages keep their order, and the wait is a few
    /// milliseconds.
    pub fn collect(&mut self) {
        for _ in 0..MESSAGES_PER_BLOCK {
            let blocked = match self.endpoint.rx.peek() {
                Ok(
                    InsertMsg::Move {
                        target: InsertTarget::Track(t),
                        ..
                    }
                    | InsertMsg::Place {
                        target: InsertTarget::Track(t),
                        ..
                    },
                ) => self.fades.get(*t).is_some_and(|f| f.pending.is_some()),
                Ok(_) => false,
                Err(_) => break,
            };
            if blocked {
                break;
            }
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
                InsertMsg::Move { target, from, to } => self.reorder(
                    target,
                    Reorder {
                        from,
                        to,
                        position: None,
                    },
                ),
                InsertMsg::Place {
                    target,
                    from,
                    to,
                    position,
                } => self.reorder(
                    target,
                    Reorder {
                        from,
                        to,
                        position: Some(position),
                    },
                ),
            }
        }
    }

    /// A reorder: on a track that is heard through plugins, under the dip; otherwise at once.
    fn reorder(&mut self, target: InsertTarget, reorder: Reorder) {
        if let InsertTarget::Track(t) = target
            && self.track_has_inserts(t)
            && let Some(fade) = self.fades.get_mut(t)
        {
            fade.pending = Some(reorder);
            fade.step = -1.0 / REORDER_FADE as f32;
            return;
        }
        self.apply_reorder(target, reorder);
    }

    fn apply_reorder(&mut self, target: InsertTarget, r: Reorder) {
        let Some(slots) = self.slots_mut(target) else {
            return;
        };
        if r.from >= MAX_INSERTS || r.to >= MAX_INSERTS {
            return;
        }
        // Rotating moves `Option<Insert>` values - pointers - and allocates nothing.
        if r.from < r.to {
            slots[r.from..=r.to].rotate_left(1);
        } else if r.to < r.from {
            slots[r.to..=r.from].rotate_right(1);
        }
        if let Some(position) = r.position
            && let Some(insert) = slots[r.to].as_mut()
        {
            insert.position = position;
        }
    }

    /// Before a track's block is rendered: a reorder whose fade-out is complete is carried out
    /// now, and the fade-in starts.
    #[inline]
    pub fn begin_track(&mut self, track: usize) {
        let Some(fade) = self.fades.get_mut(track) else {
            return;
        };
        if fade.level <= 0.0
            && let Some(r) = fade.pending.take()
        {
            fade.step = 1.0 / REORDER_FADE as f32;
            self.apply_reorder(InsertTarget::Track(track), r);
        }
    }

    /// After a track's block is rendered (built-in chain and plugins): the dip, while one runs,
    /// on the first `n` frames of both bus signals.
    #[inline]
    pub fn fade_track(&mut self, track: usize, n: usize) {
        let (Some(fade), Some([a, b])) =
            (self.fades.get_mut(track), self.track_scratch.get_mut(track))
        else {
            return;
        };
        if fade.step == 0.0 {
            return;
        }
        for i in 0..n {
            let g = fade.level;
            a.l[i] *= g;
            a.r[i] *= g;
            b.l[i] *= g;
            b.r[i] *= g;
            fade.level = (fade.level + fade.step).clamp(0.0, 1.0);
            if fade.step > 0.0 && fade.level >= 1.0 {
                fade.step = 0.0;
            }
        }
    }

    /// Whether a reorder of `track` is waiting for its fade-out.
    pub fn reorder_pending(&self, track: usize) -> bool {
        self.fades.get(track).is_some_and(|f| f.pending.is_some())
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

    /// Whether a track has an insert heard somewhere other than at the end of the chain - the
    /// built-in chain then has to be split around it (`fx::Chain::process_segment`).
    #[inline]
    pub fn track_splits_chain(&self, track: usize) -> bool {
        self.tracks.get(track).is_some_and(|s| {
            s.iter()
                .flatten()
                .any(|i| !i.shared.bypass() && i.position < InsertPosition::END)
        })
    }

    /// Whether a track has an insert heard at `position`.
    #[inline]
    pub fn track_has_inserts_at(&self, track: usize, position: InsertPosition) -> bool {
        self.tracks.get(track).is_some_and(|s| {
            s.iter()
                .flatten()
                .any(|i| !i.shared.bypass() && i.position == position)
        })
    }

    /// Run the inserts of a track at `position`, in slot order, over its scratch block for `bus`,
    /// the first `n` frames.
    #[inline]
    pub fn process_track_at(&mut self, track: usize, bus: usize, n: usize, position: InsertPosition) {
        let (Some(slots), Some(blocks)) =
            (self.tracks.get_mut(track), self.track_scratch.get_mut(track))
        else {
            return;
        };
        let block = &mut blocks[bus];
        run_slots_at(
            slots,
            Some(position),
            &mut block.l[..n],
            &mut block.r[..n],
            &self.transport,
        );
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
        run_slots(slots, &mut block.l[..n], &mut block.r[..n], &self.transport);
    }

    /// Run a bus's inserts over its scratch block, the first `n` frames.
    #[inline]
    pub fn process_bus(&mut self, bus: usize, n: usize) {
        let block = &mut self.bus_scratch[bus];
        run_slots(
            &mut self.buses[bus],
            &mut block.l[..n],
            &mut block.r[..n],
            &self.transport,
        );
    }
}

#[inline]
fn run_slots(slots: &mut Slots, l: &mut [f32], r: &mut [f32], transport: &PluginTransport) {
    run_slots_at(slots, None, l, r, transport);
}

/// Every insert of `slots` that is not bypassed - with `at`, only those at that position - in
/// slot order.
#[inline]
fn run_slots_at(
    slots: &mut Slots,
    at: Option<InsertPosition>,
    l: &mut [f32],
    r: &mut [f32],
    transport: &PluginTransport,
) {
    for insert in slots.iter_mut().flatten() {
        if insert.shared.bypass() || at.is_some_and(|p| p != insert.position) {
            continue;
        }
        insert.processor.set_transport(transport);
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

    /// Adds a constant to both channels - a DC offset, which the high-pass removes and nothing
    /// else in the chain does: proves on which side of the high-pass a plugin ran.
    pub struct Offset(pub f32);

    impl InsertProcessor for Offset {
        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            for s in left.iter_mut().chain(right.iter_mut()) {
                *s += self.0;
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
