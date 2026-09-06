//! Tracks, layers, and the loop geometry every layer of a track shares.
//!
//! A layer is one interleaved `Vec<f32>`, mono or stereo depending on what the track records: one
//! input channel means a mono buffer, a pair means a stereo one. The reasoning behind that split is
//! in [`super::frame`]. Every buffer is allocated *and zeroed* by the control thread and handed
//! over; the audio thread only writes into it, reads from it, and hands it back. It never grows,
//! never shrinks and is never zero-filled inside the callback.
//!
//! # Frames, not samples - the one unit rule of this file
//!
//! `origin`, `loop_len`, `filled`, every take boundary and every musical position count **frames**.
//! The channel count enters in exactly two places, [`Track::read`] and [`Track::write`], where a
//! frame index is turned into an offset into the interleaved buffer:
//!
//! ```text
//! offset = frame_index * channels + channel
//! ```
//!
//! Nothing else multiplies by the channel count. That is what makes the whole geometry below
//! identical for a mono and a stereo track, and it is the single mistake a stereo rebuild makes if
//! it makes one.
//!
//! # Why all layers of a track are aligned by construction
//!
//! A track owns exactly two numbers that define its grid:
//!
//! * `origin` - the musical position that loop index 0 corresponds to, set by the first take.
//! * `loop_len` - the loop length in frames, likewise set by the first take.
//!
//! Every layer, no matter which bar it was recorded in, is addressed as
//!
//! ```text
//! index = (musical position - origin) mod loop_len
//! ```
//!
//! so loop index `n` means the same musical instant in every layer. There is no per-layer offset
//! that could drift, and mixing is a plain sum over the same index. A layer recorded in bar 41 is
//! therefore aligned with the first one to the sample - not approximately, but by arithmetic. This
//! arithmetic is unchanged by stereo, because it never left the frame domain.
//!
//! Two consequences of the zeroed buffer are worth spelling out:
//!
//! * Positions an overdub never reached stay 0.0 and add nothing to the sum. No "written" bitmap is
//!   needed, and nothing has to be cleared inside the callback.
//! * While an overdub is being recorded, the sample it is about to write at index `i` is read `R`
//!   frames *before* it is written (the write pointer trails the play pointer by the latency
//!   compensation), so the running layer contributes silence on its own first pass. It becomes
//!   audible on the next pass, which is exactly what overdubbing sounds like.
//!
//! # Where the effect chain and the panner sit
//!
//! A track produces one stereo frame **per output bus** ([`super::bus`]), and [`Track::render`] is
//! the only place that happens:
//!
//! ```text
//! Ebenen-Summe + Mithoeren  ─►  (mono: auf beide Kanaele)  ─►  Kette  ─►  Panorama  ─►  Bus
//! ```
//!
//! Layers and monitoring go through the same chain, so what the musician hears while singing is
//! what the loop will sound like. The panner sits *after* the chain, the way a DAW channel strip is
//! built: the reverb is generated in the middle of the field and then placed, rather than being fed
//! a signal that is already lopsided.
//!
//! Recording never touches either of them: `process::record_input` writes the raw input samples
//! into the layer buffer, exactly as it did before effects and before stereo existed.
//!
//! # Why there is one chain *per bus* and why that is not the thing the plan warned about
//!
//! Since the loop and the monitor signal may go to **different** buses, a bus that hears only one
//! of them cannot be served by a chain that was fed both: a compressor and a reverb are not linear,
//! so `Kette(a + b)` cannot be taken apart into `Kette(a)` and `Kette(b)` afterwards. Two different
//! signals need two states, and no amount of arithmetic gets around it.
//!
//! So a track owns `BUS_COUNT` chains, one per bus, all carrying identical settings, and chain `b`
//! processes exactly the sources bus `b` is meant to hear. `docs/architektur.md` section 9 warns
//! that *"zwei parallele Ketten wuerden beim ersten Parameterwechsel auseinanderlaufen"* - that
//! warning was about two chains on the **same** signal, which would be a way for the loop and the
//! live voice to end up sounding different. Here the two carry genuinely different signals, so they
//! *have* to have different states; that is what a second bus means.
//!
//! The cost is kept where it belongs: **when both buses see the same sources, only the first chain
//! runs** and its output serves both, so the ordinary session pays exactly what it paid before.
//! The second chain is fed silence then, which its idle detection answers in a single comparison
//! per sample. The one audible consequence is that a chain waking up from silence starts without a
//! reverb tail, which is what flipping a routing switch mid-song sounds like anyway.
//!
//! # Why the latency compensation lives here and not only in the engine
//!
//! `m = k - R` is derived in [`super::process`], and `R` used to be one number for the whole
//! engine. That is right exactly as long as every source travels the same way - a microphone or a
//! guitar through the AD converter of the same interface. A plugin host such as Cantabile feeding
//! us over an ASIO router does not: its audio is already digital and never passes a converter, so
//! its input latency is *shorter*. One global value then puts one of the two sources permanently
//! beside the beat - inaudible until the takes are laid on top of each other.
//!
//! So every track carries its own [`TrackLatency`], and the engine's value is only the **default**
//! for tracks that do not. See that type for why it is two numbers rather than one.

use super::bus::{BUS_COUNT, Bus, BusSend};
use super::frame::{Channels, Frame, TrackInput, pan_gains};
use super::fx::{Chain, FxParam, FxPreset, FxSlot, FxStatus};

/// Hard ceiling of layers per track. Not a pre-allocation: layers are allocated one at a time in
/// loop length. It exists so a runaway overdub hits a clear German message instead of the memory
/// limit of the machine.
pub const MAX_LAYERS: usize = 16;

/// The latency compensation of one track, in **frames**, as two numbers that mean different
/// things.
///
/// * `measured` is what a loopback measurement found for *this* input, or `None` for "take the
///   engine's global default". A calibration overwrites it and nothing else.
/// * `trim` is a manual surcharge, added on top. A calibration never touches it.
///
/// # Why two numbers and not one
///
/// What we can measure is the way from an output of this machine back into one of its inputs. What
/// an external host has *inside* itself - its own ASIO buffer, the latency its plugins report - is
/// not on that path and cannot be measured from outside: the loopback that a router provides for a
/// Cantabile return goes around the plugin, not through it. That part is therefore a number a human
/// has to supply, by ear or from the host's own display.
///
/// Keeping it separate is what makes the two survive each other. With a single field, the next
/// calibration would silently throw the hand-dialled part away - and the musician would find out
/// weeks later, on a take that no longer sits where the previous ones do. With two, `calibrate`
/// writes `measured`, the ear writes `trim`, and the sum is what the engine subtracts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackLatency {
    /// Measured part in frames, or `None` while this track uses the engine's default.
    pub measured: Option<u32>,
    /// Manual surcharge in frames, added to the measured part. Signed, because a digital return
    /// can be *shorter* than the converter path the default was measured on.
    pub trim: i32,
}

impl TrackLatency {
    /// Takes the engine's default and adds nothing - what every track starts as.
    pub const INHERITED: TrackLatency = TrackLatency {
        measured: None,
        trim: 0,
    };

    /// A track with its own measured value.
    pub fn measured(frames: u32) -> Self {
        Self {
            measured: Some(frames),
            trim: 0,
        }
    }

    pub fn with_trim(mut self, trim: i32) -> Self {
        self.trim = trim;
        self
    }

    /// Whether this track follows the engine's default instead of a value of its own.
    #[inline]
    pub fn inherits(self) -> bool {
        self.measured.is_none()
    }

    /// The number the engine actually subtracts: measured part (or `default`) plus trim, never
    /// below zero. A negative result would mean the input arrives *before* it was played.
    #[inline]
    pub fn resolve(self, default: u64) -> u64 {
        let base = match self.measured {
            Some(frames) => u64::from(frames),
            None => default,
        };
        (base as i64 + self.trim as i64).max(0) as u64
    }
}

/// What the track is doing, for the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrackState {
    /// No layer.
    #[default]
    Empty,
    /// A recording is scheduled but its start has not been reached yet.
    Armed,
    /// The first take is running; it defines loop length and origin.
    Recording,
    /// A further layer is being recorded into an existing loop.
    Overdub,
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
            TrackState::Overdub => "Overdub",
            TrackState::Ready => "bereit",
            TrackState::Playing => "Wiedergabe",
        }
    }
}

/// One recorded layer: a buffer somebody else allocated, plus how it is mixed.
pub struct Layer {
    /// Pre-allocated and zeroed by the control thread, interleaved. Its length is the maximum loop
    /// length in frames times [`Layer::channels`].
    buffer: Vec<f32>,
    /// Channel count of this buffer. A copy of the track's - every layer of a track has the same
    /// one - kept here so a layer can answer "how many frames am I" on its own.
    channels: Channels,
    gain: f32,
    muted: bool,
}

impl Layer {
    fn new(buffer: Vec<f32>, channels: Channels) -> Self {
        Self {
            buffer,
            channels,
            gain: 1.0,
            muted: false,
        }
    }

    /// How many frames fit into this buffer.
    #[inline]
    fn frames(&self) -> u64 {
        (self.buffer.len() / self.channels.count()) as u64
    }

    /// Layer content as interleaved samples, `frames` frames long. For tests and later WAV export.
    #[cfg(test)]
    pub fn content(&self, frames: u64) -> &[f32] {
        &self.buffer[..frames as usize * self.channels.count()]
    }

    /// One channel of the content, de-interleaved. For tests that check the two sides separately.
    #[cfg(test)]
    pub fn channel(&self, channel: usize, frames: u64) -> Vec<f32> {
        let n = self.channels.count();
        let c = channel.min(n - 1);
        (0..frames as usize).map(|i| self.buffer[i * n + c]).collect()
    }

    #[cfg(test)]
    pub fn channels(&self) -> Channels {
        self.channels
    }
}

/// A take that is already scheduled while the previous one is still finishing.
///
/// This exists because of the latency compensation: when the output reaches the end of a take, its
/// last `R` samples are still travelling in from the interface, so the take cannot be closed yet.
/// Recording eight bars and overdubbing the next eight without a pause - the normal way to build a
/// loop - would otherwise collide with itself. The buffer is fetched at scheduling time, in the
/// control thread's supply, and only becomes a layer when the running take really ends.
pub struct PendingTake {
    start: u64,
    end: Option<u64>,
    defines_loop: bool,
    buffer: Vec<f32>,
}

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

pub struct Track {
    /// Which device input channel or channel pair this track records. Its channel count is the
    /// channel count of every layer buffer this track will ever get.
    input: TrackInput,
    channels: Channels,
    /// Position in the stereo field, -1.0 hard left to +1.0 hard right. See [`pan_gains`] for the
    /// law and for why its centre is unity rather than -3 dB.
    pan: f32,
    /// Pan a fresh start would have, restored by "alles loeschen".
    pan_default: f32,
    /// Room for `MAX_LAYERS` from the start, so pushing a layer never reallocates.
    layers: Vec<Layer>,
    /// Musical position that loop index 0 corresponds to.
    origin: u64,
    /// Loop length in **frames**; 0 means "no loop yet".
    loop_len: u64,
    /// Frames written contiguously during a loop-defining take. Only that take needs it: it is
    /// what bounds the loop length when the take is closed.
    filled: u64,
    take: Option<Take>,
    pending: Option<PendingTake>,
    playing: bool,
    monitor: bool,
    /// Monitoring state a fresh start would have, restored by "alles loeschen".
    monitor_default: bool,
    /// Peak of each recorded input channel since the last status snapshot. On a mono track only
    /// index 0 is ever written.
    input_peak: [f32; 2],
    /// Peak this track contributed to the mix bus since the last status snapshot, per side: the
    /// sum of its audible layers, panned, measured **before** the effect chain.
    ///
    /// Two deliberate choices in that sentence. The chain is outside, so the meter keeps saying how
    /// loud the *take* is rather than how much makeup gain the preset adds - a level that changes
    /// when a preset is loaded is useless for setting a level. The panner is inside, because a
    /// hard-panned track that showed the same level on both meters would simply be lying about
    /// where it sits. Monitoring is not part of it either; that path never touches a layer.
    output_peak: [f32; 2],
    /// Which buses this track's **loop playback** feeds.
    loop_send: BusSend,
    /// The loop routing a fresh start would have, restored by "alles loeschen".
    loop_send_default: BusSend,
    /// Which buses this track's **monitored live input** feeds, independent of the loop. As a rule
    /// the headphones and not the room: the audience usually has the singer through the PA's own
    /// channel, and hearing him twice is worse than not hearing him through the looper at all.
    monitor_send: BusSend,
    monitor_send_default: BusSend,
    /// One effect chain per bus, all with identical settings; chain `b` processes what bus `b` is
    /// meant to hear. See the module comment for why a single chain cannot serve two buses that see
    /// different sources, and why only the first one runs when they see the same.
    fx: [Chain; BUS_COUNT],
    /// Latency compensation of this track as it was configured - the two numbers a human sets.
    latency: TrackLatency,
    /// The same, resolved against the engine's default and added up: the `R` of `m = k - R` for
    /// this track, in frames.
    ///
    /// Kept as a plain number so the recording loop reads one field per track instead of doing the
    /// arithmetic once per frame. It is recomputed whenever the configuration or the default
    /// changes, which happens in the audio thread but only on a command - never per sample.
    latency_frames: u64,
}

impl Track {
    /// Built by the control thread - the layer vector and the chains' buffers (delay lines, reverb
    /// combs) are the only allocations, and they happen here.
    pub fn new(input: TrackInput, monitor: bool, pan: f32, sample_rate: u32) -> Self {
        let pan = pan.clamp(-1.0, 1.0);
        Self {
            input,
            channels: input.channels(),
            pan,
            pan_default: pan,
            loop_send: BusSend::BOTH,
            loop_send_default: BusSend::BOTH,
            monitor_send: BusSend::MONITOR,
            monitor_send_default: BusSend::MONITOR,
            layers: Vec::with_capacity(MAX_LAYERS),
            origin: 0,
            loop_len: 0,
            filled: 0,
            take: None,
            pending: None,
            playing: false,
            monitor,
            monitor_default: monitor,
            input_peak: [0.0; 2],
            output_peak: [0.0; 2],
            fx: [Chain::new(sample_rate), Chain::new(sample_rate)],
            latency: TrackLatency::INHERITED,
            // Resolved by `EngineCore::new`, which is the only place that knows the default.
            latency_frames: 0,
        }
    }

    /// A mono track on one input channel, centred. The shorthand the tests and the mono default
    /// setup use.
    #[cfg(test)]
    pub fn mono(input_channel: usize, monitor: bool, sample_rate: u32) -> Self {
        Self::new(TrackInput::Mono(input_channel), monitor, 0.0, sample_rate)
    }

    /// Give this track its own latency compensation while it is being built in the control thread.
    /// It is resolved against the engine's default in [`super::process::EngineCore::new`].
    pub fn with_latency(mut self, latency: TrackLatency) -> Self {
        self.latency = latency;
        self
    }

    /// How this track's compensation is configured: the measured part and the manual surcharge.
    #[inline]
    pub fn latency(&self) -> TrackLatency {
        self.latency
    }

    /// The `R` this track's recording is shifted by, in frames.
    #[inline]
    pub fn latency_frames(&self) -> u64 {
        self.latency_frames
    }

    /// New configuration; `default` is the engine's global value, needed because a track without a
    /// measured value of its own follows it.
    pub fn set_latency(&mut self, latency: TrackLatency, default: u64) {
        self.latency = latency;
        self.latency_frames = latency.resolve(default);
    }

    /// Recompute the effective value after the engine's default changed (or was first supplied).
    pub fn resolve_latency(&mut self, default: u64) {
        self.latency_frames = self.latency.resolve(default);
    }

    /// The chain that answers for the settings. All of them carry the same ones; this is the one
    /// that is read back.
    #[inline]
    pub fn fx(&self) -> &Chain {
        &self.fx[0]
    }

    /// State of the chain for the status snapshot.
    #[inline]
    pub fn fx_status(&mut self) -> FxStatus {
        self.fx[0].status()
    }

    /// Every chain of this track. Settings are applied to all of them, always, so a parameter can
    /// never end up meaning two different things on two buses.
    #[inline]
    pub fn set_fx_bypass(&mut self, on: bool) {
        for chain in self.fx.iter_mut() {
            chain.set_bypass(on);
        }
    }

    #[inline]
    pub fn set_fx_enabled(&mut self, slot: FxSlot, on: bool) {
        for chain in self.fx.iter_mut() {
            chain.set_enabled(slot, on);
        }
    }

    #[inline]
    pub fn set_fx_param(&mut self, param: FxParam) {
        for chain in self.fx.iter_mut() {
            chain.set_param(param);
        }
    }

    #[inline]
    pub fn load_fx_preset(&mut self, preset: FxPreset) {
        for chain in self.fx.iter_mut() {
            chain.load_preset(preset);
        }
    }

    #[inline]
    pub fn set_quarter_samples(&mut self, quarter: f64) {
        for chain in self.fx.iter_mut() {
            chain.set_quarter_samples(quarter);
        }
    }

    /// Which buses this track's loop playback feeds.
    #[inline]
    pub fn loop_send(&self) -> BusSend {
        self.loop_send
    }

    #[inline]
    pub fn set_loop_send(&mut self, send: BusSend) {
        self.loop_send = send;
    }

    /// Which buses this track's monitored live input feeds - independent of the loop, because the
    /// two answer different questions: where the recording belongs, and where the musician needs to
    /// hear himself.
    #[inline]
    pub fn monitor_send(&self) -> BusSend {
        self.monitor_send
    }

    #[inline]
    pub fn set_monitor_send(&mut self, send: BusSend) {
        self.monitor_send = send;
    }

    /// The routing a track starts with, used when it is built from a configuration.
    #[must_use]
    pub fn with_sends(mut self, loop_send: BusSend, monitor_send: BusSend) -> Self {
        self.loop_send = loop_send;
        self.loop_send_default = loop_send;
        self.monitor_send = monitor_send;
        self.monitor_send_default = monitor_send;
        self
    }

    /// Whether both buses see the same set of sources, so one chain run serves both.
    ///
    /// Monitoring that is switched off contributes silence to every bus, so its routing does not
    /// split anything then - which is why a session where nobody is monitoring costs exactly what
    /// it cost with a single bus.
    #[inline(always)]
    fn buses_share_sources(&self) -> bool {
        let uniform = |send: BusSend| send.on(Bus::Main) == send.on(Bus::Monitor);
        uniform(self.loop_send)
            && (!self.monitor || self.monitor_send.is_silent() || uniform(self.monitor_send))
    }

    /// What this track contributes to each output bus at musical position `pos`.
    ///
    /// `monitor` is this track's live input, already fanned out to stereo and scaled by the monitor
    /// gain, or silence when monitoring is off. Per bus, the sources that bus is meant to hear are
    /// summed *before* the chain - so a musician singing into a track that goes through a reverb
    /// hears that reverb - and the panner comes last, as in a DAW channel strip.
    ///
    /// `collapsed` says that both buses leave on the same device channels, which is what a
    /// two-output interface forces. There is then **one** way out and therefore one mix: every
    /// source that is heard on either bus is summed exactly once, into the first bus, and the
    /// second one stays silent. Rendering both and letting the placement add them up would put a
    /// track assigned to both buses 6 dB over one assigned to a single bus, and plugging in a
    /// second output pair would change the balance of the first.
    ///
    /// The output meter is unchanged by all of this: it still measures the audible layers, panned,
    /// before the chain, and it does not care which bus they end up on. It answers "how loud is the
    /// take", which is a property of the take.
    #[inline]
    pub fn render(&mut self, pos: u64, monitor: Frame, collapsed: bool) -> [Frame; BUS_COUNT] {
        let dry = if self.playing {
            self.read(pos)
        } else {
            Frame::SILENT
        };
        let (gain_l, gain_r) = pan_gains(self.pan);
        self.note_output_peak(Frame::new(dry.l * gain_l, dry.r * gain_r));

        let place = |wet: Frame| Frame::new(wet.l * gain_l, wet.r * gain_r);
        let source = |bus: Bus, loop_send: BusSend, monitor_send: BusSend| {
            let mut src = Frame::SILENT;
            if loop_send.on(bus) {
                src = src.added(dry);
            }
            if monitor_send.on(bus) {
                src = src.added(monitor);
            }
            src
        };

        if collapsed {
            let mut src = Frame::SILENT;
            if !self.loop_send.is_silent() {
                src = src.added(dry);
            }
            if !self.monitor_send.is_silent() {
                src = src.added(monitor);
            }
            let wet = place(self.fx[0].process(src));
            self.fx[1].process(Frame::SILENT);
            return [wet, Frame::SILENT];
        }

        if self.buses_share_sources() {
            let src = source(Bus::Main, self.loop_send, self.monitor_send);
            let wet = place(self.fx[0].process(src));
            // Keep the idle chain settled rather than frozen mid-tail; one comparison per sample
            // once it has run dry.
            self.fx[1].process(Frame::SILENT);
            return [wet; BUS_COUNT];
        }

        let mut out = [Frame::SILENT; BUS_COUNT];
        for bus in Bus::ALL {
            let src = source(bus, self.loop_send, self.monitor_send);
            out[bus.index()] = place(self.fx[bus.index()].process(src));
        }
        out
    }

    #[inline]
    pub fn input(&self) -> TrackInput {
        self.input
    }

    /// Channel count of this track's loop buffers: 1 for a microphone, 2 for a stereo source.
    #[inline]
    pub fn channels(&self) -> Channels {
        self.channels
    }

    #[inline]
    pub fn pan(&self) -> f32 {
        self.pan
    }

    #[inline]
    pub fn set_pan(&mut self, pan: f32) {
        self.pan = pan.clamp(-1.0, 1.0);
    }

    #[inline]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    #[inline]
    pub fn loop_len(&self) -> u64 {
        self.loop_len
    }

    /// Musical position loop index 0 corresponds to; 0 while no take has defined a loop.
    ///
    /// The control thread needs it to place a further layer on exactly this track's grid instead of
    /// on the global one - see `engine::schedule`.
    #[inline]
    pub fn origin(&self) -> u64 {
        self.origin
    }

    #[inline]
    pub fn filled(&self) -> u64 {
        self.filled
    }

    #[inline]
    pub fn has_content(&self) -> bool {
        !self.layers.is_empty() && self.loop_len > 0
    }

    #[inline]
    pub fn playing(&self) -> bool {
        self.playing
    }

    #[inline]
    pub fn set_playing(&mut self, on: bool) {
        self.playing = on;
    }

    #[inline]
    pub fn monitor(&self) -> bool {
        self.monitor
    }

    #[inline]
    pub fn set_monitor(&mut self, on: bool) {
        self.monitor = on;
    }

    #[inline]
    pub fn take(&self) -> Option<Take> {
        self.take
    }

    /// Bit `i` is set when layer `i` is muted. Fits `MAX_LAYERS` and travels in the status snapshot.
    pub fn muted_mask(&self) -> u32 {
        let mut mask = 0u32;
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.muted {
                mask |= 1 << i;
            }
        }
        mask
    }

    pub fn state(&self, musical_input_pos: u64) -> TrackState {
        match self.take {
            Some(take) if musical_input_pos < take.start => TrackState::Armed,
            Some(take) if take.defines_loop => TrackState::Recording,
            Some(_) => TrackState::Overdub,
            None if self.layers.is_empty() => TrackState::Empty,
            None if self.playing => TrackState::Playing,
            None => TrackState::Ready,
        }
    }

    /// Note the peak of one recorded input channel. `channel` is the buffer channel, not the
    /// device channel.
    #[inline]
    pub fn note_input_peak(&mut self, channel: usize, magnitude: f32) {
        let slot = &mut self.input_peak[channel.min(1)];
        if magnitude > *slot {
            *slot = magnitude;
        }
    }

    #[inline]
    pub fn take_input_peak(&mut self) -> [f32; 2] {
        std::mem::replace(&mut self.input_peak, [0.0; 2])
    }

    #[inline]
    pub fn note_output_peak(&mut self, frame: Frame) {
        for c in 0..2 {
            let magnitude = frame.channel(c).abs();
            if magnitude > self.output_peak[c] {
                self.output_peak[c] = magnitude;
            }
        }
    }

    #[inline]
    pub fn take_output_peak(&mut self) -> [f32; 2] {
        std::mem::replace(&mut self.output_peak, [0.0; 2])
    }

    /// Sum of all audible layers at musical position `pos`, as a stereo frame.
    ///
    /// A mono track's sum is fanned out to both channels here, so everything downstream - chain,
    /// panner, bus - sees a stereo frame and needs no case distinction. See [`super::frame`] for
    /// why that fan-out is at unity rather than at -3 dB.
    ///
    /// Positions that were never written return silence rather than stale memory, because every
    /// buffer arrives zeroed from the control thread.
    #[inline]
    pub fn read(&self, pos: u64) -> Frame {
        if self.loop_len == 0 || pos < self.origin {
            return Frame::SILENT;
        }
        let frame_index = ((pos - self.origin) % self.loop_len) as usize;
        let n = self.channels.count();
        // The one place a frame index becomes a buffer offset. See the module comment.
        let base = frame_index * n;
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for layer in &self.layers {
            if layer.muted {
                continue;
            }
            // `loop_len` never exceeds a buffer's frame count, so this always hits; `get` keeps a
            // hypothetical mismatch from panicking inside the audio callback.
            if let Some(&v) = layer.buffer.get(base) {
                left += v * layer.gain;
            }
            if n == 2 {
                if let Some(&v) = layer.buffer.get(base + 1) {
                    right += v * layer.gain;
                }
            }
        }
        if n == 2 {
            Frame::new(left, right)
        } else {
            Frame::mono(left)
        }
    }

    /// Write one frame of the running take. `pos` is the *musical* position of the frame, i.e.
    /// latency compensation has already been applied by the caller. On a mono track only the left
    /// channel of `frame` is stored.
    #[inline]
    pub fn write(&mut self, pos: u64, frame: Frame) {
        let Some(take) = self.take else {
            return;
        };
        let channels = self.channels;
        let Some(layer) = self.layers.get_mut(take.layer) else {
            return;
        };
        let capacity = layer.frames();
        let index = if take.defines_loop {
            let index = pos.wrapping_sub(self.origin);
            if index >= capacity {
                return;
            }
            // A loop-defining take is written strictly sequentially, so this is the only value
            // `index` can have.
            debug_assert_eq!(index, self.filled, "Luecke im Loop-Puffer");
            self.filled = index + 1;
            index
        } else {
            if self.loop_len == 0 {
                return;
            }
            (pos - self.origin) % self.loop_len
        };
        let n = channels.count();
        // The second and last place a frame index becomes a buffer offset.
        let base = index as usize * n;
        for c in 0..n {
            if let Some(slot) = layer.buffer.get_mut(base + c) {
                *slot = frame.channel(c);
            }
        }
    }

    /// Musical position at which the running take has to end at the latest, if any.
    ///
    /// A loop-defining take is bounded by the buffer; an overdub is bounded by one pass through the
    /// loop, so it can never overwrite what it has just recorded. Both in frames.
    #[inline]
    pub fn take_limit(&self) -> Option<u64> {
        let take = self.take?;
        let layer = self.layers.get(take.layer)?;
        if take.defines_loop {
            Some(take.start + layer.frames())
        } else {
            Some(take.start + self.loop_len)
        }
    }

    /// Start the first take of a new loop. The caller has already returned the old layers.
    ///
    /// The buffer has to be long enough for `frames * channels` samples; the caller
    /// (`process::take_spare`) picks it from the pool of the right channel count.
    pub fn begin_loop_take(&mut self, start: u64, buffer: Vec<f32>) {
        debug_assert!(self.layers.len() < MAX_LAYERS);
        debug_assert_eq!(
            buffer.len() % self.channels.count(),
            0,
            "Puffer passt nicht zur Kanalzahl des Tracks"
        );
        self.origin = start;
        self.loop_len = 0;
        self.filled = 0;
        self.playing = false;
        self.layers.push(Layer::new(buffer, self.channels));
        self.take = Some(Take {
            start,
            end: None,
            layer: self.layers.len() - 1,
            defines_loop: true,
        });
    }

    /// Start a further layer on the existing grid.
    pub fn begin_overdub_take(&mut self, start: u64, buffer: Vec<f32>) {
        debug_assert!(self.layers.len() < MAX_LAYERS);
        debug_assert_eq!(
            buffer.len() % self.channels.count(),
            0,
            "Puffer passt nicht zur Kanalzahl des Tracks"
        );
        self.layers.push(Layer::new(buffer, self.channels));
        self.take = Some(Take {
            start,
            end: None,
            layer: self.layers.len() - 1,
            defines_loop: false,
        });
    }

    /// Is a take running whose end is already known, i.e. one that is only waiting for its last
    /// `R` samples to arrive? A further take may be scheduled behind it.
    #[inline]
    pub fn take_is_finishing(&self) -> bool {
        self.take.map(|t| t.end.is_some()).unwrap_or(false)
    }

    #[inline]
    pub fn pending_defines_loop(&self) -> bool {
        self.pending.as_ref().map(|p| p.defines_loop).unwrap_or(false)
    }

    /// Queue a take behind the running one. Returns the buffer of a pending take that was replaced.
    pub fn set_pending(
        &mut self,
        start: u64,
        defines_loop: bool,
        buffer: Vec<f32>,
    ) -> Option<Vec<f32>> {
        let previous = self.pending.take().map(|p| p.buffer);
        self.pending = Some(PendingTake {
            start,
            end: None,
            defines_loop,
            buffer,
        });
        previous
    }

    /// Start the queued take now that the previous one is closed. Returns its buffer instead when
    /// it cannot be started after all.
    pub fn promote_pending(&mut self) -> Option<Vec<f32>> {
        let pending = self.pending.take()?;
        if self.layers.len() >= MAX_LAYERS {
            return Some(pending.buffer);
        }
        if pending.defines_loop {
            self.begin_loop_take(pending.start, pending.buffer);
        } else {
            self.begin_overdub_take(pending.start, pending.buffer);
        }
        if let Some(end) = pending.end {
            self.set_take_end(end);
        }
        None
    }

    /// Hand back the buffer of a queued take, cancelling it.
    pub fn take_pending_buffer(&mut self) -> Option<Vec<f32>> {
        self.pending.take().map(|p| p.buffer)
    }

    /// Announce where the running take ends, before the last sample has been written.
    ///
    /// Needed because the write pointer trails the play pointer by the compensated latency: when
    /// the output reaches the end of the take, the last `R` samples are still on their way in.
    /// Publishing the length early is what lets playback start seamlessly at the loop boundary;
    /// the zeroed buffer keeps the not-yet-written tail silent instead of stale.
    pub fn set_take_end(&mut self, end: u64) {
        // A stop that arrives after a further take was scheduled belongs to that one - the control
        // thread always sends start and stop of a take as a pair, in that order.
        if let Some(pending) = self.pending.as_mut() {
            pending.end = Some(end.max(pending.start));
            return;
        }
        let capacity = self.take_capacity();
        if let Some(take) = self.take.as_mut() {
            let end = end.max(take.start);
            take.end = Some(end);
            if take.defines_loop {
                self.loop_len = (end - take.start).min(capacity);
            }
        }
    }

    /// Close the take. `end` is the musical position just after its last sample.
    pub fn finish_take(&mut self, end: u64) {
        if let Some(take) = self.take.take()
            && take.defines_loop
        {
            self.loop_len = end.saturating_sub(self.origin).min(self.filled);
        }
    }

    /// Frames the running take's buffer can hold.
    fn take_capacity(&self) -> u64 {
        self.take
            .and_then(|t| self.layers.get(t.layer))
            .map(Layer::frames)
            .unwrap_or(0)
    }

    /// Hand back the last layer, so the caller can return its buffer to the control thread.
    ///
    /// Emptying a track resets its geometry: the next take defines a new loop.
    pub fn pop_layer(&mut self) -> Option<Vec<f32>> {
        let layer = self.layers.pop()?;
        if self.layers.is_empty() {
            self.origin = 0;
            self.loop_len = 0;
            self.filled = 0;
            self.playing = false;
        }
        Some(layer.buffer)
    }

    /// Remove one layer by index, handing its buffer back. Later layers move down by one.
    pub fn remove_layer(&mut self, index: usize) -> Option<Vec<f32>> {
        if index >= self.layers.len() {
            return None;
        }
        // Moves at most `MAX_LAYERS` `Vec` headers - a memmove of a few hundred bytes, no
        // allocation and no deallocation.
        let layer = self.layers.remove(index);
        if self.layers.is_empty() {
            self.origin = 0;
            self.loop_len = 0;
            self.filled = 0;
            self.playing = false;
        }
        Some(layer.buffer)
    }

    pub fn set_layer_muted(&mut self, index: usize, muted: bool) -> bool {
        match self.layers.get_mut(index) {
            Some(layer) => {
                layer.muted = muted;
                true
            }
            None => false,
        }
    }

    pub fn set_layer_gain(&mut self, index: usize, gain: f32) -> bool {
        match self.layers.get_mut(index) {
            Some(layer) => {
                layer.gain = gain.clamp(0.0, 4.0);
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub fn layer(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    /// Cancel a running take without touching the layers that were finished before it.
    pub fn cancel_take(&mut self) {
        self.take = None;
    }

    /// Restore the monitoring state a fresh start would have. Called by "alles loeschen" after the
    /// layers have been returned.
    ///
    /// The **effect chain is deliberately left alone**. A preset is a property of the channel -
    /// the microphone, the pickup, the room - not of the material that happens to be recorded in
    /// it. Between two songs a musician presses "alles leeren" and would be furious to find his
    /// voice back to dry. What is left of a reverb tail dies away on its own, because a cleared
    /// track feeds its chain nothing but zeros.
    ///
    /// **The latency compensation is left alone for the same reason, and a stronger one.** It
    /// describes the cable, the converter and the router this input hangs on; none of that changed
    /// because the loops were emptied. Resetting it would mean re-measuring after every song.
    pub fn restore_defaults(&mut self) {
        self.monitor = self.monitor_default;
        // The pan is part of the arrangement, not of the channel: two mono tracks placed left and
        // right are a setup decision, and emptying the loops must not drag both back to the middle.
        // It is restored to what the track was *started* with, which is what "wie frisch gestartet"
        // means everywhere else in this function.
        self.pan = self.pan_default;
        // The bus routing is part of the setup in the same way: which bus a track belongs on was
        // decided when the stage was wired, not by the material. It goes back to what the track was
        // started with, not to some blanket value.
        self.loop_send = self.loop_send_default;
        self.monitor_send = self.monitor_send_default;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with_layer(frames: usize) -> Track {
        let mut t = Track::mono(0, false, 48_000);
        t.begin_loop_take(1_000, vec![0.0; frames]);
        t
    }

    /// A stereo track with a buffer of `frames` frames, i.e. `2 * frames` samples.
    fn stereo_track_with_layer(frames: usize) -> Track {
        let mut t = Track::new(TrackInput::Stereo { left: 2, right: 3 }, false, 0.0, 48_000);
        t.begin_loop_take(1_000, vec![0.0; frames * 2]);
        t
    }

    #[test]
    fn writes_are_sequential_and_readable() {
        let mut t = track_with_layer(100);
        for i in 0..40u64 {
            t.write(1_000 + i, Frame::mono(i as f32));
        }
        t.finish_take(1_040);
        assert_eq!(t.loop_len(), 40);
        assert_eq!(t.read(1_000), Frame::mono(0.0));
        assert_eq!(t.read(1_039), Frame::mono(39.0));
        // Wrap-around: frame 40 of the loop is frame 0 again.
        assert_eq!(t.read(1_040), Frame::mono(0.0));
        assert_eq!(t.read(1_041), Frame::mono(1.0));
        assert_eq!(t.read(1_000 + 40 * 7 + 13), Frame::mono(13.0));
    }

    /// The same thing on a stereo track, and the point of the whole exercise: the two channels
    /// carry different material and never swap places, however far into the loop one reads.
    #[test]
    fn a_stereo_track_keeps_its_two_channels_apart() {
        let mut t = stereo_track_with_layer(100);
        assert_eq!(t.channels(), Channels::Stereo);
        for i in 0..40u64 {
            t.write(1_000 + i, Frame::new(i as f32, 100.0 + i as f32));
        }
        t.finish_take(1_040);
        assert_eq!(t.loop_len(), 40, "die Loop-Laenge zaehlt Frames, nicht Samples");
        assert_eq!(t.read(1_000), Frame::new(0.0, 100.0));
        assert_eq!(t.read(1_039), Frame::new(39.0, 139.0));
        assert_eq!(t.read(1_040), Frame::new(0.0, 100.0), "Naht");
        assert_eq!(t.read(1_000 + 40 * 7 + 13), Frame::new(13.0, 113.0));
        let layer = t.layer(0).expect("Ebene");
        assert_eq!(layer.channels(), Channels::Stereo);
        assert_eq!(
            layer.content(40).len(),
            80,
            "interleaved: 40 Frames sind 80 Samples"
        );
        assert_eq!(layer.channel(0, 40)[13], 13.0);
        assert_eq!(layer.channel(1, 40)[13], 113.0);
    }

    #[test]
    fn unwritten_positions_are_silent() {
        let mut t = track_with_layer(100);
        for i in 0..10u64 {
            t.write(1_000 + i, Frame::mono(1.0));
        }
        // Pretend the take was closed at +50 although only 10 frames arrived: the loop can only
        // be as long as what was actually written.
        t.finish_take(1_050);
        assert_eq!(t.loop_len(), 10);
        assert_eq!(t.read(1_009), Frame::mono(1.0));
        assert_eq!(t.read(1_010), Frame::mono(1.0)); // wrapped, not stale memory
    }

    #[test]
    fn writes_beyond_capacity_are_dropped() {
        let mut t = track_with_layer(8);
        for i in 0..20u64 {
            t.write(1_000 + i, Frame::mono(i as f32));
        }
        assert_eq!(t.filled(), 8);

        // And the same bound in frames on a stereo track, whose buffer is twice as long in
        // samples: eight frames, not sixteen.
        let mut t = stereo_track_with_layer(8);
        for i in 0..20u64 {
            t.write(1_000 + i, Frame::new(i as f32, i as f32));
        }
        assert_eq!(t.filled(), 8, "die Grenze zaehlt Frames");
    }

    #[test]
    fn clearing_the_last_layer_resets_the_geometry() {
        let mut t = track_with_layer(64);
        t.write(1_000, Frame::mono(0.5));
        t.finish_take(1_001);
        assert!(t.has_content());
        let buffer = t.pop_layer().expect("Puffer kommt zurueck");
        assert_eq!(buffer.len(), 64);
        assert!(!t.has_content());
        assert_eq!(t.loop_len(), 0);
        assert_eq!(t.read(1_000), Frame::SILENT);
        assert_eq!(t.layer_count(), 0);

        // A stereo track hands back a buffer of twice the length - the pool has to get its own
        // kind back, or the next stereo take would be handed a mono buffer.
        let mut t = stereo_track_with_layer(64);
        t.write(1_000, Frame::new(0.5, 0.25));
        t.finish_take(1_001);
        let buffer = t.pop_layer().expect("Puffer kommt zurueck");
        assert_eq!(buffer.len(), 128);
    }

    /// Three layers, the later ones started in the middle of the loop: every one of them has to
    /// address the same musical instant with the same index.
    #[test]
    fn layers_share_one_grid_regardless_of_where_they_started() {
        let mut t = Track::mono(0, false, 48_000);
        t.begin_loop_take(1_000, vec![0.0; 64]);
        for i in 0..10u64 {
            t.write(1_000 + i, Frame::mono(1.0));
        }
        t.set_take_end(1_010);
        t.finish_take(1_010);
        assert_eq!(t.loop_len(), 10);

        // Second layer starts three loop passes later, in the middle of the loop.
        let start = 1_000 + 3 * 10 + 4;
        t.begin_overdub_take(start, vec![0.0; 64]);
        for i in 0..10u64 {
            t.write(start + i, Frame::mono(10.0));
        }
        t.finish_take(start + 10);

        // Every loop position now carries 1.0 + 10.0, wherever the overdub happened to begin.
        for i in 0..10u64 {
            assert_eq!(t.read(1_000 + i), Frame::mono(11.0), "Loop-Index {i}");
            assert_eq!(
                t.read(1_000 + 77 * 10 + i),
                Frame::mono(11.0),
                "Loop-Index {i}, spaeter"
            );
        }
    }

    /// The same grid property on a stereo track, with the two channels carrying different
    /// material: an offset of one *sample* instead of one frame would swap the sides.
    #[test]
    fn stereo_layers_share_one_grid_without_swapping_the_sides() {
        let mut t = stereo_track_with_layer(64);
        for i in 0..10u64 {
            t.write(1_000 + i, Frame::new(1.0, 2.0));
        }
        t.set_take_end(1_010);
        t.finish_take(1_010);

        let start = 1_000 + 3 * 10 + 4;
        t.begin_overdub_take(start, vec![0.0; 128]);
        for i in 0..10u64 {
            t.write(start + i, Frame::new(10.0, 20.0));
        }
        t.finish_take(start + 10);

        for i in 0..10u64 {
            assert_eq!(t.read(1_000 + i), Frame::new(11.0, 22.0), "Loop-Index {i}");
            assert_eq!(
                t.read(1_000 + 77 * 10 + i),
                Frame::new(11.0, 22.0),
                "Loop-Index {i}, spaeter"
            );
        }
    }

    #[test]
    fn muting_and_gain_change_the_sum_only() {
        let mut t = Track::mono(0, false, 48_000);
        t.begin_loop_take(0, vec![0.0; 16]);
        for i in 0..4u64 {
            t.write(i, Frame::mono(1.0));
        }
        t.set_take_end(4);
        t.finish_take(4);
        t.begin_overdub_take(0, vec![0.0; 16]);
        for i in 0..4u64 {
            t.write(i, Frame::mono(2.0));
        }
        t.finish_take(4);

        assert_eq!(t.read(0), Frame::mono(3.0));
        assert!(t.set_layer_muted(1, true));
        assert_eq!(
            t.read(0),
            Frame::mono(1.0),
            "stummer Layer faellt aus der Summe"
        );
        assert_eq!(t.muted_mask(), 0b10);
        assert!(t.set_layer_muted(1, false));
        assert!(t.set_layer_gain(1, 0.5));
        assert_eq!(t.read(0), Frame::mono(2.0));
        assert!(!t.set_layer_gain(7, 0.5), "Layer 7 gibt es nicht");
    }

    /// The panner, at the positions a musician actually uses. A mono track in the centre is
    /// equally loud on both sides; hard left leaves the right side digitally silent.
    #[test]
    fn a_mono_track_sits_in_the_middle_until_it_is_panned() {
        let mut t = track_with_layer(16);
        for i in 0..4u64 {
            t.write(1_000 + i, Frame::mono(0.5));
        }
        t.set_take_end(1_004);
        t.finish_take(1_004);
        t.set_playing(true);

        assert_eq!(t.pan(), 0.0, "ein Track startet in der Mitte");
        assert_eq!(
            t.render(1_000, Frame::SILENT, false)[0],
            Frame::new(0.5, 0.5),
            "Mitte: gleich laut auf beiden Seiten"
        );

        t.set_pan(-1.0);
        assert_eq!(
            t.render(1_001, Frame::SILENT, false)[0],
            Frame::new(0.5, 0.0),
            "ganz links laesst rechts still"
        );

        t.set_pan(1.0);
        assert_eq!(
            t.render(1_002, Frame::SILENT, false)[0],
            Frame::new(0.0, 0.5),
            "ganz rechts laesst links still"
        );

        t.set_pan(-0.5);
        assert_eq!(t.render(1_003, Frame::SILENT, false)[0], Frame::new(0.5, 0.25));

        // Out of range is clamped, not wrapped.
        t.set_pan(-9.0);
        assert_eq!(t.pan(), -1.0);
    }

    /// A stereo track's pan is a balance: at the centre both sides pass at unity, so a stereo
    /// source is not 3 dB quieter than a mono one just for being stereo.
    #[test]
    fn a_stereo_track_keeps_both_sides_at_unity_in_the_centre() {
        let mut t = stereo_track_with_layer(16);
        t.write(1_000, Frame::new(0.4, 0.8));
        t.set_take_end(1_001);
        t.finish_take(1_001);
        t.set_playing(true);
        assert_eq!(t.render(1_000, Frame::SILENT, false)[0], Frame::new(0.4, 0.8));
        t.set_pan(-1.0);
        assert_eq!(t.render(1_000, Frame::SILENT, false)[0], Frame::new(0.4, 0.0));
    }

    /// The output meter is measured after the panner: a hard-panned track that showed level on
    /// both sides would be lying about where it is.
    #[test]
    fn the_output_meter_follows_the_panner() {
        let mut t = track_with_layer(16);
        t.write(1_000, Frame::mono(0.5));
        t.set_take_end(1_001);
        t.finish_take(1_001);
        t.set_playing(true);
        t.set_pan(-1.0);
        t.render(1_000, Frame::SILENT, false);
        assert_eq!(t.take_output_peak(), [0.5, 0.0]);
        assert_eq!(
            t.take_output_peak(),
            [0.0, 0.0],
            "der Peak wird beim Lesen geleert"
        );
    }

    /// The arithmetic behind "gemessener Anteil plus manueller Zuschlag", including the two cases
    /// that make it worth having two fields: a track that follows the default still takes its
    /// surcharge, and a surcharge survives a new measurement.
    #[test]
    fn a_track_latency_is_its_measured_part_plus_its_trim() {
        let default = 827u64;

        let inherited = TrackLatency::INHERITED;
        assert!(inherited.inherits());
        assert_eq!(inherited.resolve(default), 827, "ohne eigenen Wert gilt die Vorgabe");

        let own = TrackLatency::measured(512);
        assert!(!own.inherits());
        assert_eq!(own.resolve(default), 512);

        // The surcharge rides on the default as well as on a measured value.
        assert_eq!(inherited.with_trim(64).resolve(default), 891);
        assert_eq!(own.with_trim(64).resolve(default), 576);
        assert_eq!(own.with_trim(-100).resolve(default), 412);

        // A new measurement replaces the measured part and leaves the surcharge standing - the
        // whole point of keeping them apart.
        let after_calibration = TrackLatency {
            measured: Some(540),
            ..own.with_trim(64)
        };
        assert_eq!(after_calibration.trim, 64);
        assert_eq!(after_calibration.resolve(default), 604);

        // A compensation cannot go below zero: that would mean the input arrived before it was
        // played.
        assert_eq!(TrackLatency::measured(10).with_trim(-999).resolve(default), 0);
        assert_eq!(inherited.with_trim(-9_999).resolve(default), 0);
    }

    /// The track keeps the resolved number ready, so the recording loop reads a field instead of
    /// doing arithmetic per frame - and emptying the track must not touch it.
    #[test]
    fn a_track_resolves_its_latency_once_and_keeps_it_when_it_is_cleared() {
        let mut t = Track::mono(0, false, 48_000).with_latency(TrackLatency::measured(512).with_trim(20));
        assert_eq!(t.latency_frames(), 0, "vor dem Aufloesen steht noch nichts fest");
        t.resolve_latency(827);
        assert_eq!(t.latency_frames(), 532);

        t.set_latency(TrackLatency::INHERITED, 827);
        assert_eq!(t.latency_frames(), 827);
        assert!(t.latency().inherits());

        t.set_latency(TrackLatency::measured(900), 827);
        t.restore_defaults();
        assert_eq!(
            t.latency_frames(),
            900,
            "die Latenz beschreibt die Verkabelung, nicht das Material"
        );
    }

    #[test]
    fn the_input_meter_has_one_value_per_recorded_channel() {
        let mut t = stereo_track_with_layer(16);
        t.note_input_peak(0, 0.25);
        t.note_input_peak(1, 0.75);
        t.note_input_peak(0, 0.1);
        assert_eq!(t.take_input_peak(), [0.25, 0.75]);
        assert_eq!(t.take_input_peak(), [0.0, 0.0]);
    }
}
