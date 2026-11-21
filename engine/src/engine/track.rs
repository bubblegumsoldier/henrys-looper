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
use super::fx::smooth::Smoothed;
use super::fx::{Chain, FxParam, FxPreset, FxSlot, FxStatus};
use super::grid::{LoopGrid, PendingGeom};
use super::insert::Spread;
use super::midiloop::MidiClip;

/// Upper end of a channel fader, linear: +12 dB. Enough to lift a quiet take to the level of the
/// others; more is a gain-staging problem the fader should not hide.
pub const MAX_TRACK_GAIN: f32 = 3.981_071_7;

/// Time constant of the fader smoother. Mute and solo ride on the same ramp, so a switch is a
/// 10 ms fade rather than a click - short enough to feel instant, long enough to be silent.
const FADER_MS: f32 = 10.0;

/// Hard ceiling of layers per track. Not a pre-allocation: layers are allocated one at a time in
/// loop length. It exists so a runaway overdub hits a clear German message instead of the memory
/// limit of the machine.
pub const MAX_LAYERS: usize = 16;

/// Length of every declicking fade and crossfade of loop playback and monitoring, in
/// milliseconds. See [`Track::play_frame`] for where they sit and why there.
pub const EDGE_MS: f64 = 10.0;

/// Most frames a fade can have, whatever the sample rate: 10 ms at 96 kHz fit. It sizes the
/// pre-roll every layer keeps and the input history every track keeps.
pub const EDGE_MAX: usize = 1024;

/// Samples one layer's pre-roll takes: `EDGE_MAX` stereo frames.
const PRE_SAMPLES: usize = 2 * EDGE_MAX;

/// Switches of a track's audibility that can be pending at once. Arming happens at most one fade
/// length ahead, so more than two never overlap in practice; a full set folds its oldest.
const MAX_SWITCHES: usize = 4;

/// Frames of one fade at `sample_rate`.
pub fn edge_frames(sample_rate: u32) -> u64 {
    ((f64::from(sample_rate) * EDGE_MS / 1000.0).round() as u64).clamp(1, EDGE_MAX as u64)
}

/// Rising half of a raised cosine over `n` frames, sampled at the centre of frame `d`: never
/// exactly 0 or 1 inside the window, and `fade_in(d) + fade_out(d) == 1` - an amplitude-
/// complementary crossfade. Chosen over an equal-power one because what clicks here is low-
/// frequency content, and that is nearly the same on both sides of a few milliseconds: summing
/// two correlated signals at equal power would bulge by up to 3 dB, which is a thump of its own.
/// The cosine shape has no corner at either end, so the fade adds no splatter of its own.
#[inline(always)]
pub fn fade_in(d: u64, n: u64) -> f32 {
    let x = (d as f32 + 0.5) / n.max(1) as f32;
    0.5 - 0.5 * (std::f32::consts::PI * x).cos()
}

#[inline(always)]
pub fn fade_out(d: u64, n: u64) -> f32 {
    1.0 - fade_in(d, n)
}

/// Index of musical position `pos` in a loop of `len` frames whose index 0 sits at `origin`,
/// also for positions *before* `origin` (the lead-in of a re-anchored loop).
#[inline(always)]
fn loop_index(pos: u64, origin: u64, len: u64) -> u64 {
    if pos >= origin {
        (pos - origin) % len
    } else {
        let back = (origin - pos) % len;
        if back == 0 { 0 } else { len - back }
    }
}

#[inline(always)]
fn scaled(f: Frame, g: f32) -> Frame {
    Frame::new(f.l * g, f.r * g)
}

/// Frames from index `i` forward to index `target` in a loop of `len`: `1..=len`, `len` when they
/// are the same index.
#[inline(always)]
fn ahead_to(target: u64, i: u64, len: u64) -> u64 {
    if target > i { target - i } else { target + len - i }
}

/// One change of a track's audibility, on the read axis.
///
/// `at` is where the change *counts* (the first position that belongs to the new state). The gain
/// ramps from `g0` to the new state over `w0..w1`. Armed ahead of time the window ends on `at`, so
/// a loop that stops at `at` is silent from `at` on and one that starts at `at` has its first frame
/// untouched; an untimed or late change ramps from where it was applied.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Switch {
    at: u64,
    on: bool,
    w0: u64,
    w1: u64,
    g0: f32,
}

/// The crossfade from the old grid to the new one around a re-anchoring at `grid.restart`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RestartFade {
    at: u64,
    w0: u64,
    w1: u64,
}

/// What the audibility envelope says at one position.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Env {
    Silent,
    Full,
    /// Inside a switch window. `aligned` is set for a window armed ahead of time from a whole
    /// state: then every layer whose seam is the switch position takes only the part of its
    /// crossfade that belongs to the side that sounds (see [`Part`]).
    Ramp {
        gain: f32,
        aligned: Option<(u64, bool)>,
    },
}

/// Which share of each layer's signal a position contributes, see [`Track::sum_layers`].
#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    /// Everything: body plus the lead-in of the next pass, i.e. the seam crossfade.
    Steady,
    /// Everything, scaled.
    Gain(f32),
    /// The last `fade` frames before a stop (or before the old grid ends) at `E`: a layer whose seam
    /// is `E` contributes its body only - it fades out through its own seam crossfade, and the next
    /// pass, which will not be heard, adds nothing. Every other layer fades out.
    Before(u64),
    /// The last `fade` frames before a start (or before the new grid begins) at `E`: a layer whose
    /// seam is `E` contributes only the pre-roll leading into it, so its first frame plays
    /// untouched. Every other layer fades in.
    After(u64),
}

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
            TrackState::Empty => "Empty",
            TrackState::Armed => "Armed",
            TrackState::Recording => "Record",
            TrackState::Overdub => "Overdub",
            TrackState::Ready => "Ready",
            TrackState::Playing => "Play",
        }
    }
}

/// What one layer holds: interleaved samples, or the notes of a MIDI take. Both are allocated by
/// the control thread and travel back to it the same way; the audio thread never creates or drops
/// either.
pub enum LayerStore {
    /// Pre-allocated and zeroed, interleaved. Its length is the maximum loop length in frames times
    /// the track's channel count.
    Audio(Vec<f32>),
    /// A pre-allocated note clip (`super::midiloop`).
    Midi(Box<MidiClip>),
}

impl From<Vec<f32>> for LayerStore {
    fn from(buffer: Vec<f32>) -> Self {
        LayerStore::Audio(buffer)
    }
}

impl From<Box<MidiClip>> for LayerStore {
    fn from(clip: Box<MidiClip>) -> Self {
        LayerStore::Midi(clip)
    }
}

impl LayerStore {
    /// Samples of an audio buffer, events a clip can hold.
    pub fn len(&self) -> usize {
        match self {
            LayerStore::Audio(buffer) => buffer.len(),
            LayerStore::Midi(clip) => clip.capacity(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_midi(&self) -> bool {
        matches!(self, LayerStore::Midi(_))
    }
}

/// One recorded layer: storage somebody else allocated, plus how it is mixed.
///
/// # The seam, the end and the pre-roll
///
/// A take begins and ends in the middle of whatever the musician plays, so its first and its last
/// sample rarely meet - and every pass of a loop plays the one right after the other. Played
/// back raw that is a step at every wrap, and with the low-frequency content of a piezo guitar a
/// clearly audible click. So every layer remembers three things beside its samples:
///
/// * `seam` - the loop index its take began at (0 for the take that defined the loop, the grid
///   position of an overdub otherwise);
/// * `pre` - the input that arrived in the `fade` frames *before* that first sample, copied from
///   the track's input history when the take wrote its first frame;
/// * `end` - the index right after its last sample: the seam again for a whole pass, somewhere
///   else for an overdub cut short.
///
/// On playback the last `fade` frames before the seam are crossfaded from the layer's own body
/// into its pre-roll, which runs, sample by sample, into the first frame of the take - it *is* the
/// signal that led into it. The first frame and everything after it play untouched; the downbeat
/// a loop starts on keeps its attack. Before an `end` that is not the seam the body fades out.
pub struct Layer {
    store: LayerStore,
    /// Channel count of an audio buffer. A copy of the track's - every layer of a track has the
    /// same one - kept here so a layer can answer "how many frames am I" on its own.
    channels: Channels,
    gain: f32,
    muted: bool,
    seam: u64,
    end: u64,
    /// Which of the track's pre-roll slots is this layer's (`Track::pre_rolls`). Kept outside the
    /// layer so a layer stays a few dozen bytes: the mix walks every layer every frame.
    pre_slot: usize,
    pre_ready: bool,
    /// Frames this layer's take wrote, and the index of the last one - what `end` comes from.
    written: u64,
    last: u64,
}

impl Layer {
    fn new(store: LayerStore, channels: Channels, pre_slot: usize) -> Self {
        Self {
            store,
            channels,
            gain: 1.0,
            muted: false,
            seam: 0,
            end: 0,
            pre_slot,
            pre_ready: false,
            written: 0,
            last: 0,
        }
    }

    /// The loop index this layer's take began at.
    #[inline]
    pub fn seam(&self) -> u64 {
        self.seam
    }

    /// One frame of this layer at loop index `i`, split into the part that belongs to the pass
    /// containing `i` (its body, faded out before an early end) and the lead-in of the pass that
    /// begins at the seam, plus how far ahead that seam is. Silent on a MIDI layer. `pre` is the
    /// layer's pre-roll: interleaved stereo, `pre[2k], pre[2k+1]` the input `edge - k` frames
    /// before the take's first frame.
    #[inline(always)]
    fn parts(&self, i: u64, len: u64, fade: u64, edge: u64, pre: &[f32]) -> (Frame, Frame, u64) {
        let samples = self.samples();
        let n = self.channels.count();
        let base = i as usize * n;
        let l = samples.get(base).copied().unwrap_or(0.0);
        let r = if n == 2 {
            samples.get(base + 1).copied().unwrap_or(0.0)
        } else {
            l
        };
        let mut body = Frame::new(l * self.gain, r * self.gain);
        let to_seam = ahead_to(self.seam, i, len);
        if to_seam > fade && self.end == self.seam {
            // Everywhere but the last fade before the seam of a whole pass: the sample itself.
            return (body, Frame::SILENT, to_seam);
        }
        let to_end = ahead_to(self.end, i, len);
        if self.end != self.seam && to_end <= fade {
            let g = fade_out(fade - to_end, fade);
            body = Frame::new(body.l * g, body.r * g);
        }
        let lead = if to_seam <= fade {
            let k = (edge - to_seam) as usize;
            let g = fade_in(fade - to_seam, fade) * self.gain;
            let fo = fade_out(fade - to_seam, fade);
            // A whole pass ends on its seam: the body fades out against the pre-roll there.
            if self.end == self.seam {
                body = Frame::new(body.l * fo, body.r * fo);
            }
            let at = |n: usize| pre.get(n).copied().unwrap_or(0.0);
            Frame::new(at(2 * k) * g, at(2 * k + 1) * g)
        } else {
            Frame::SILENT
        };
        (body, lead, to_seam)
    }

    /// How many frames this layer can cover: the buffer length of an audio layer, the maximum loop
    /// length the clip was handed of a MIDI one.
    #[inline]
    fn frames(&self) -> u64 {
        match &self.store {
            LayerStore::Audio(buffer) => (buffer.len() / self.channels.count()) as u64,
            LayerStore::Midi(clip) => clip.max_frames(),
        }
    }

    /// The samples of an audio layer; empty for a MIDI one.
    #[inline]
    fn samples(&self) -> &[f32] {
        match &self.store {
            LayerStore::Audio(buffer) => buffer,
            LayerStore::Midi(_) => &[],
        }
    }

    #[inline]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    #[inline]
    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn clip(&self) -> Option<&MidiClip> {
        match &self.store {
            LayerStore::Midi(clip) => Some(clip),
            LayerStore::Audio(_) => None,
        }
    }

    pub fn clip_mut(&mut self) -> Option<&mut MidiClip> {
        match &mut self.store {
            LayerStore::Midi(clip) => Some(clip),
            LayerStore::Audio(_) => None,
        }
    }

    /// Layer content as interleaved samples, `frames` frames long. For tests and later WAV export.
    #[cfg(test)]
    pub fn content(&self, frames: u64) -> &[f32] {
        let samples = self.samples();
        &samples[..(frames as usize * self.channels.count()).min(samples.len())]
    }

    /// One channel of the content, de-interleaved. For tests that check the two sides separately.
    #[cfg(test)]
    pub fn channel(&self, channel: usize, frames: u64) -> Vec<f32> {
        let n = self.channels.count();
        let c = channel.min(n - 1);
        let samples = self.samples();
        (0..frames as usize)
            .map(|i| samples.get(i * n + c).copied().unwrap_or(0.0))
            .collect()
    }

    #[cfg(test)]
    pub fn channels(&self) -> Channels {
        self.channels
    }
}

pub use super::grid::Take;

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
    /// Origin, loop length and the take state - the part that does not care what a layer holds.
    grid: LoopGrid,
    /// Storage of a take queued behind the running one (`grid.pending` is its geometry). Fetched
    /// at scheduling time, in the control thread's supply, and only becomes a layer when the
    /// running take really ends: when the output reaches the end of a take its last `R` samples are
    /// still travelling in, so recording eight bars and overdubbing the next eight without a pause
    /// would otherwise collide with itself.
    pending_store: Option<LayerStore>,
    /// Whether this track's takes store notes (a MIDI loop) rather than its instrument's sound.
    midi_loop: bool,
    playing: bool,
    /// True while the transport stands: the loop is not heard, whatever `playing` says.
    held: bool,
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
    /// Frames the loop is read ahead to make up for plugin latency. See [`Track::render_unpanned`].
    playback_advance: u64,
    /// The channel fader, linear `0..=MAX_TRACK_GAIN`. After the chain and the plugin inserts,
    /// before the panner - the place a DAW channel strip has it. Never reaches a recording.
    gain: f32,
    /// Channel mute and solo. Listening only: what is recorded is the raw input either way.
    muted: bool,
    soloed: bool,
    /// What actually multiplies the strip: `gain`, or 0 while muted or soloed away. One smoother
    /// for all three, so fader moves, mute and solo are click-free alike. At rest it is exactly
    /// the target, so a fader at 1.0 multiplies by exactly 1.0 - bit-identical to no fader.
    audible: Smoothed,
    /// Peak of what this strip sends, post-fader and post-panner, the louder bus per side.
    strip_peak: [f32; 2],
    /// On an instrument track: the fixed delay `C` plus the instrument's own latency, in frames -
    /// what its recording is taken back by before the trim is added. See `super::source`.
    source_base: u64,
    /// A re-anchoring is armed (`LoopGrid::restart`) and the track is to sound from its position
    /// on - judged on the **read** axis, so a loop read ahead for plugin or chain latency begins
    /// with its very first frame instead of losing the first `playback_advance` of them. The
    /// `StartPlay` sent with it makes it permanent; a stop clears it.
    restart_play: bool,
    /// Frames of every fade at this track's sample rate ([`edge_frames`]).
    edge: u64,
    /// The last `edge` input frames this track recorded or could have recorded, by musical
    /// position - what a take's pre-roll is copied from. Stereo interleaved, allocated here.
    history: Vec<f32>,
    /// One past the newest position in `history`, and the first one of the unbroken run it holds.
    history_end: u64,
    history_from: u64,
    /// Slot of `history_end` in `history`, counted along so the per-frame path has no division.
    history_slot: usize,
    /// The pre-roll of every layer, `MAX_LAYERS` slots of [`PRE_SAMPLES`], allocated here; bit `n`
    /// of `pre_free` says slot `n` is unused.
    pre_rolls: Vec<f32>,
    pre_free: u32,
    /// Set when a take has just written its first frame (its start), until the engine asks.
    fresh_take: Option<u64>,
    /// Audibility of the loop before the first pending switch (playing and not held).
    sound_base: bool,
    switches: [Switch; MAX_SWITCHES],
    switch_count: usize,
    restart_fade: Option<RestartFade>,
    /// A play state armed ahead (`arm_playing`): it becomes `playing` when the output reaches the
    /// position.
    play_pending: Option<(u64, bool)>,
    /// Monitoring fades over `edge` input frames from `monitor_from` to `monitor_to`, starting at
    /// input index `monitor_k0`.
    monitor_k0: u64,
    monitor_from: f32,
    monitor_to: f32,
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
            grid: LoopGrid::default(),
            pending_store: None,
            midi_loop: false,
            playing: false,
            held: false,
            monitor,
            monitor_default: monitor,
            input_peak: [0.0; 2],
            output_peak: [0.0; 2],
            fx: [Chain::new(sample_rate), Chain::new(sample_rate)],
            latency: TrackLatency::INHERITED,
            // Resolved by `EngineCore::new`, which is the only place that knows the default.
            latency_frames: 0,
            playback_advance: 0,
            gain: 1.0,
            muted: false,
            soloed: false,
            audible: Smoothed::new(1.0, FADER_MS, sample_rate),
            strip_peak: [0.0; 2],
            source_base: 0,
            restart_play: false,
            edge: edge_frames(sample_rate),
            history: vec![0.0; 2 * EDGE_MAX],
            history_end: 0,
            history_from: 0,
            history_slot: 0,
            pre_rolls: vec![0.0; MAX_LAYERS * PRE_SAMPLES],
            pre_free: (1u32 << MAX_LAYERS) - 1,
            fresh_take: None,
            sound_base: false,
            switches: [Switch {
                at: 0,
                on: false,
                w0: 0,
                w1: 0,
                g0: 0.0,
            }; MAX_SWITCHES],
            switch_count: 0,
            restart_fade: None,
            play_pending: None,
            monitor_k0: 0,
            monitor_from: if monitor { 1.0 } else { 0.0 },
            monitor_to: if monitor { 1.0 } else { 0.0 },
        }
        .instrument_defaults()
    }

    /// An instrument track is heard in the room as well as in the headphones: its "live input" is
    /// music, not a voice the PA already carries. Every other track keeps its defaults.
    fn instrument_defaults(mut self) -> Self {
        if self.is_instrument() {
            self.monitor_send = BusSend::BOTH;
            self.monitor_send_default = BusSend::BOTH;
        }
        self
    }

    /// Whether this track's sound comes from an instrument in its source slot rather than from an
    /// input channel.
    #[inline(always)]
    pub fn is_instrument(&self) -> bool {
        matches!(self.input, TrackInput::Instrument)
    }

    /// On an instrument track: the fixed delay plus the instrument's latency, recomputed by the
    /// engine every block. The trim of [`TrackLatency`] is added on top, the measured part is
    /// meaningless here - nothing travels through a converter.
    #[inline]
    pub fn set_source_base(&mut self, base: u64) {
        if self.source_base != base {
            self.source_base = base;
            self.latency_frames = self.instrument_latency();
        }
    }

    #[inline]
    fn instrument_latency(&self) -> u64 {
        (self.source_base as i64 + self.latency.trim as i64).max(0) as u64
    }

    /// The channel fader, linear.
    #[inline]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    #[inline]
    pub fn muted(&self) -> bool {
        self.muted
    }

    #[inline]
    pub fn soloed(&self) -> bool {
        self.soloed
    }

    /// New fader value; anything that is not a finite number is ignored rather than let into the
    /// audio path. `any_solo` is the engine-wide answer to "is any track soloed".
    #[inline]
    pub fn set_gain(&mut self, gain: f32, any_solo: bool) {
        if gain.is_finite() {
            self.gain = gain.clamp(0.0, MAX_TRACK_GAIN);
        }
        self.retarget(any_solo);
    }

    #[inline]
    pub fn set_muted(&mut self, on: bool, any_solo: bool) {
        self.muted = on;
        self.retarget(any_solo);
    }

    /// Only the flag; the engine retargets every track afterwards, because a solo changes what
    /// the others may be heard at.
    #[inline]
    pub fn set_soloed(&mut self, on: bool) {
        self.soloed = on;
    }

    /// Point the smoother at what this strip should be heard at now.
    #[inline]
    pub fn retarget(&mut self, any_solo: bool) {
        let silenced = self.muted || (any_solo && !self.soloed);
        self.audible.set(if silenced { 0.0 } else { self.gain });
    }

    /// Advance the fader by one frame and return its factor. Called exactly once per frame,
    /// however many buses the strip feeds.
    #[inline(always)]
    pub fn fader_next(&mut self) -> f32 {
        self.audible.next()
    }

    /// Where the fader is right now, without advancing it - for the recorder, which places live
    /// input through the strip at a different moment than the strip runs.
    #[inline(always)]
    pub fn fader_level(&self) -> f32 {
        self.audible.value()
    }

    /// Fader, then panner, for one bus frame. `g` is this frame's [`Track::fader_next`].
    #[inline(always)]
    pub fn strip(&self, wet: Frame, g: f32) -> Frame {
        self.place(Frame::new(wet.l * g, wet.r * g))
    }

    #[inline]
    pub fn note_strip_peak(&mut self, frame: Frame) {
        for c in 0..2 {
            let magnitude = frame.channel(c).abs();
            if magnitude > self.strip_peak[c] {
                self.strip_peak[c] = magnitude;
            }
        }
    }

    #[inline]
    pub fn take_strip_peak(&mut self) -> [f32; 2] {
        std::mem::replace(&mut self.strip_peak, [0.0; 2])
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
        self.resolve_latency(default);
    }

    /// Recompute the effective value after the engine's default changed (or was first supplied).
    pub fn resolve_latency(&mut self, default: u64) {
        self.latency_frames = if self.is_instrument() {
            self.instrument_latency()
        } else {
            self.latency.resolve(default)
        };
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

    /// A hand on an effect's switch - see [`crate::engine::fx::Chain::touch_enabled`].
    #[inline]
    pub fn touch_fx_enabled(&mut self, slot: FxSlot, on: bool) {
        for chain in self.fx.iter_mut() {
            chain.touch_enabled(slot, on);
        }
    }

    /// A hand on a knob - see [`crate::engine::fx::Chain::touch_param`].
    #[inline]
    pub fn touch_fx_param(&mut self, param: FxParam) {
        for chain in self.fx.iter_mut() {
            chain.touch_param(param);
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
            && (!self.monitor_active()
                || self.monitor_send.is_silent()
                || uniform(self.monitor_send))
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
        let (wet, _) = self.render_unpanned(pos, monitor, collapsed);
        let g = self.fader_next();
        let out = [self.strip(wet[0], g), self.strip(wet[1], g)];
        self.note_strip_peak(out[0]);
        self.note_strip_peak(out[1]);
        out
    }

    /// Apply this track's panner to one frame. The last step of the channel strip.
    #[inline(always)]
    pub fn place(&self, wet: Frame) -> Frame {
        let (gain_l, gain_r) = pan_gains(self.pan);
        Frame::new(wet.l * gain_l, wet.r * gain_r)
    }

    /// Frames the loop playback is read ahead of the output position, to make up for the latency of
    /// this track's plugin inserts (`super::insert`) and of its built-in chain (the octaver,
    /// `super::fx::octaver`). Zero without either.
    #[inline]
    pub fn set_playback_advance(&mut self, frames: u64) {
        self.playback_advance = frames;
    }

    #[inline]
    pub fn playback_advance(&self) -> u64 {
        self.playback_advance
    }

    /// Everything [`Track::render`] does except the panner, plus how the two bus signals relate -
    /// the stage the plugin inserts are run on (after the built-in chain, before the panner).
    ///
    /// The loop is read `playback_advance` frames ahead: a plugin that reports latency `L` delays
    /// what it gets by `L`, so feeding it the loop `L` frames early puts the loop back on the click.
    /// The live input cannot be read ahead and is late by `L` - the price of a plugin with lookahead.
    #[inline]
    pub fn render_unpanned(
        &mut self,
        pos: u64,
        monitor: Frame,
        collapsed: bool,
    ) -> ([Frame; BUS_COUNT], Spread) {
        let (src, spread) = self.render_sources(pos, monitor, collapsed);
        match spread {
            Spread::Collapsed | Spread::Shared => {
                let wet = self.fx[0].process(src[0]);
                // Keep the idle chain settled rather than frozen mid-tail; one comparison per
                // sample once it has run dry.
                self.fx[1].process(Frame::SILENT);
                let other = if spread == Spread::Shared { wet } else { Frame::SILENT };
                ([wet, other], spread)
            }
            Spread::Split => (
                [self.fx[0].process(src[0]), self.fx[1].process(src[1])],
                Spread::Split,
            ),
        }
    }

    /// The chain of bus `bus` (0 = Main), for a block renderer that runs it in segments around
    /// plugin inserts (`super::insert`, `fx::Chain::process_segment`).
    #[inline]
    pub fn chain_mut(&mut self, bus: usize) -> &mut Chain {
        &mut self.fx[bus.min(BUS_COUNT - 1)]
    }

    /// What goes *into* each bus's chain at musical position `pos`: the loop (read ahead by
    /// `playback_advance`) and the monitored input, summed per bus as [`Track::render`] describes.
    /// Collapsed: everything in the first bus, the second silent. Shared: the same in both.
    #[inline]
    pub fn render_sources(
        &mut self,
        pos: u64,
        monitor: Frame,
        collapsed: bool,
    ) -> ([Frame; BUS_COUNT], Spread) {
        let ahead = pos + self.playback_advance;
        let dry = self.play_frame(ahead);
        let (gain_l, gain_r) = pan_gains(self.pan);
        self.note_output_peak(Frame::new(dry.l * gain_l, dry.r * gain_r));

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
            return ([src, Frame::SILENT], Spread::Collapsed);
        }

        if self.buses_share_sources() {
            let src = source(Bus::Main, self.loop_send, self.monitor_send);
            return ([src; BUS_COUNT], Spread::Shared);
        }

        let mut out = [Frame::SILENT; BUS_COUNT];
        for bus in Bus::ALL {
            out[bus.index()] = source(bus, self.loop_send, self.monitor_send);
        }
        (out, Spread::Split)
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
        self.grid.loop_len
    }

    /// Musical position loop index 0 corresponds to; 0 while no take has defined a loop.
    ///
    /// The control thread needs it to place a further layer on exactly this track's grid instead of
    /// on the global one - see `engine::schedule`.
    #[inline]
    pub fn origin(&self) -> u64 {
        // The newest geometry: a take that begins now, the status and the free-mode scheduler all
        // mean the loop as it will be from here on.
        self.grid.latest_origin()
    }

    #[inline]
    pub fn filled(&self) -> u64 {
        self.grid.filled
    }

    #[inline]
    pub fn has_content(&self) -> bool {
        !self.layers.is_empty() && self.grid.loop_len > 0
    }

    #[inline]
    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Start or stop the loop **at once**, without a fade: for what happens together with its
    /// content changing (a track that is cleared, a new loop that begins) and for the MIDI loop,
    /// which has no samples to fade. Audio playback that is started or stopped as music goes
    /// through [`Track::arm_playing`] or [`Track::fade_playing`].
    #[inline]
    pub fn set_playing(&mut self, on: bool) {
        self.play_pending = None;
        self.playing = on;
        if !on {
            self.restart_play = false;
        }
        self.sound_base = on && !self.held;
        self.switch_count = 0;
        if !on {
            self.restart_fade = None;
        }
    }

    /// Start or stop the loop so that it counts from read position `at` on: armed at least one fade
    /// length ahead (`now` is the read position about to be rendered), a stop fades out over the
    /// frames *before* `at` and is silent from `at` on, a start fades in what leads into `at` and
    /// plays `at` itself untouched. Armed later than that, the fade is shortened or - when `at` has
    /// passed - begins at `now`.
    ///
    /// Only the sound is early. The play *state* - what the status reports and the engine's own
    /// logic asks - changes when the output reaches `output_at`, exactly as before the fades
    /// ([`Track::settle_play`]).
    pub fn arm_playing(&mut self, on: bool, at: u64, now: u64, output_at: u64) {
        let target = on && !self.held;
        self.arm_switch(at, target, now);
        self.play_pending = Some((output_at, on));
    }

    /// Start or stop the loop now, with a fade that begins at read position `now`: an untimed stop
    /// ("Stop all" acts on its own sample, but the engine's `Stop` is untimed).
    pub fn fade_playing(&mut self, on: bool, now: u64) {
        self.play_pending = None;
        self.playing = on;
        if !on {
            self.restart_play = false;
        }
        self.arm_switch(now, on && !self.held, now);
    }

    /// Take over a play state armed ahead once the output has reached its position.
    #[inline]
    pub fn settle_play(&mut self, out_pos: u64) {
        if let Some((at, on)) = self.play_pending
            && at <= out_pos
        {
            self.play_pending = None;
            self.playing = on;
            if !on {
                self.restart_play = false;
            }
        }
    }

    /// Whether a MIDI loop plays at read position `pos`: playing, or about to be by an armed
    /// re-anchoring whose position the read-ahead has reached. (An audio loop asks
    /// [`Track::play_frame`], which fades.)
    #[inline(always)]
    pub fn sounds_at(&self, pos: u64) -> bool {
        self.playing || (self.restart_play && self.grid.restart.is_some_and(|at| pos >= at))
    }

    /// Whether the loop is heard at read position `pos` according to what is armed: the state
    /// after every switch whose position has been reached. No ramp, no content check.
    #[inline(always)]
    fn audible_at(&self, pos: u64) -> bool {
        let mut on = self.sound_base;
        for s in &self.switches[..self.switch_count] {
            if s.at <= pos {
                on = s.on;
            }
        }
        on
    }

    /// The audibility envelope at read position `pos`, for fades of length `fade`.
    #[inline(always)]
    fn env(&self, pos: u64, fade: u64) -> Env {
        let mut current: Option<&Switch> = None;
        for s in &self.switches[..self.switch_count] {
            if s.w0 <= pos {
                current = Some(s);
            }
        }
        let Some(s) = current else {
            return if self.sound_base { Env::Full } else { Env::Silent };
        };
        if pos >= s.w1 {
            return if s.on { Env::Full } else { Env::Silent };
        }
        let n = s.w1 - s.w0;
        let target = if s.on { 1.0 } else { 0.0 };
        let gain = s.g0 + (target - s.g0) * fade_in(pos - s.w0, n);
        let whole = s.g0 == 1.0 - target;
        let aligned = (whole && s.w1 == s.at && n == fade).then_some((s.at, s.on));
        Env::Ramp { gain, aligned }
    }

    /// The envelope's gain at `pos`, as a number.
    #[inline]
    fn env_gain(&self, pos: u64) -> f32 {
        match self.env(pos, self.fade_len()) {
            Env::Silent => 0.0,
            Env::Full => 1.0,
            Env::Ramp { gain, .. } => gain,
        }
    }

    /// Record a change of audibility to `on`, counting from `at`. A change at or before a pending
    /// one replaces it (the later command wins, as in the engine's waiting room); a change to the
    /// state that is armed anyway does nothing.
    fn arm_switch(&mut self, at: u64, on: bool, now: u64) {
        while self.switch_count > 0 && self.switches[self.switch_count - 1].at >= at {
            self.switch_count -= 1;
        }
        let armed = match self.switch_count {
            0 => self.sound_base,
            n => self.switches[n - 1].on,
        };
        if armed == on {
            return;
        }
        let fade = self.fade_len();
        let (w0, w1, at) = if at >= now + fade {
            (at - fade, at, at)
        } else if at > now {
            (now, at, at)
        } else {
            (now, now + fade, now)
        };
        let g0 = self.env_gain(w0);
        if self.switch_count == MAX_SWITCHES {
            self.sound_base = self.switches[0].on;
            self.switches.copy_within(1.., 0);
            self.switch_count -= 1;
        }
        self.switches[self.switch_count] = Switch {
            at,
            on,
            w0,
            w1,
            g0,
        };
        self.switch_count += 1;
    }

    /// Forget switches and fades that lie wholly before read position `pos`. Called once per block
    /// with the lowest position anything still reads (the dry stem reads without the read-ahead).
    #[inline]
    pub fn fold_edges(&mut self, pos: u64, input_index: u64) {
        let mut done = 0;
        while done < self.switch_count
            && self.switches[done].w1 <= pos
            && self.switches[done].at <= pos
        {
            self.sound_base = self.switches[done].on;
            done += 1;
        }
        if done > 0 {
            self.switches.copy_within(done..self.switch_count, 0);
            self.switch_count -= done;
        }
        if self.restart_fade.is_some_and(|r| r.w1 <= pos) {
            self.restart_fade = None;
        }
        if input_index >= self.monitor_k0 + self.edge {
            self.monitor_from = self.monitor_to;
        }
    }

    /// Fade length for this track's loop: the configured one - or none at all for a loop shorter
    /// than eight fades (80 ms), which no musician records; only tests build loops that short, and
    /// they read them sample by sample.
    #[inline(always)]
    pub fn fade_len(&self) -> u64 {
        if self.grid.loop_len >= 8 * self.edge {
            self.edge
        } else {
            0
        }
    }

    /// Frames of one fade at this track's sample rate.
    #[inline]
    pub fn edge(&self) -> u64 {
        self.edge
    }

    /// What the loop plays at read position `pos`: the sum of its audible layers with every seam
    /// crossfaded, every start, stop and re-anchoring faded. This is the one function both the
    /// mix and the dry stem read the loop through, so the stem holds what was heard.
    ///
    /// Where the fades sit, and why there:
    ///
    /// * **Seams** - inside each layer, the last `fade` frames before its seam crossfade into its
    ///   pre-roll ([`Layer`]). The first frame of a pass plays untouched.
    /// * **Start at `P`** (armed ahead) - the `fade` frames before `P` fade in what leads into `P`:
    ///   a layer whose seam is `P` (a loop that starts from its top, the usual case) contributes
    ///   only its pre-roll, so the attack on the downbeat is untouched, and nothing of the pass
    ///   before - which was not heard - leaks in. Any other layer fades in from its own content.
    /// * **Stop at `P`** (armed ahead) - silent from `P` on, the `fade` frames before fade out; a
    ///   layer whose seam is `P` simply runs out through its own seam fade.
    /// * **Re-anchoring at `P`** - the old grid fades out and the new one fades in over the same
    ///   frames before `P`, each with the start/stop rule above, so the new top plays untouched.
    /// * **Late or untimed** changes ("Stop all", the transport) fade from where they act.
    #[inline]
    pub fn play_frame(&self, pos: u64) -> Frame {
        let len = self.grid.loop_len;
        if len == 0 || self.layers.is_empty() || self.midi_loop {
            return Frame::SILENT;
        }
        let fade = self.fade_len();
        if let Some(r) = self.restart_fade
            && pos >= r.w0
            && pos < r.w1
        {
            let old = r.at.checked_sub(1).is_some_and(|p| self.audible_at(p));
            let new = self.audible_at(r.at);
            let n = r.w1 - r.w0;
            let d = pos - r.w0;
            let whole = n == fade && r.w1 == r.at;
            let mut out = Frame::SILENT;
            if old {
                let part = if whole {
                    Part::Before(r.at)
                } else {
                    Part::Gain(fade_out(d, n))
                };
                out = out.added(self.sum_layers(self.grid.origin, pos, part, fade));
            }
            if new {
                let part = if whole {
                    Part::After(r.at)
                } else {
                    Part::Gain(fade_in(d, n))
                };
                out = out.added(self.sum_layers(r.at, pos, part, fade));
            }
            return out;
        }
        let origin = self.grid.origin_at(pos);
        if pos < origin {
            return Frame::SILENT;
        }
        let part = match self.env(pos, fade) {
            Env::Silent => return Frame::SILENT,
            Env::Full => Part::Steady,
            Env::Ramp {
                aligned: Some((at, true)),
                ..
            } => Part::After(at),
            Env::Ramp {
                aligned: Some((at, false)),
                ..
            } => Part::Before(at),
            Env::Ramp { gain, .. } => Part::Gain(gain),
        };
        self.sum_layers(origin, pos, part, fade)
    }

    /// The audible layers at read position `pos` on the grid whose index 0 is `origin`, each
    /// contributing what `part` says.
    #[inline(always)]
    fn sum_layers(&self, origin: u64, pos: u64, part: Part, fade: u64) -> Frame {
        let len = self.grid.loop_len;
        let i = loop_index(pos, origin, len);
        if part == Part::Steady {
            // The common case, kept as lean as the plain read it replaced: one comparison per
            // layer decides whether this index is in its seam fade at all.
            let n = self.channels.count();
            let base = i as usize * n;
            let (mut left, mut right) = (0.0f32, 0.0f32);
            for layer in &self.layers {
                if layer.muted {
                    continue;
                }
                if layer.end == layer.seam && ahead_to(layer.seam, i, len) > fade {
                    let samples = layer.samples();
                    let l = samples.get(base).copied().unwrap_or(0.0) * layer.gain;
                    left += l;
                    right += if n == 2 {
                        samples.get(base + 1).copied().unwrap_or(0.0) * layer.gain
                    } else {
                        l
                    };
                } else {
                    let pre = self.pre_of(layer.pre_slot);
                    let (body, lead, _) = layer.parts(i, len, fade, self.edge, pre);
                    let v = body.added(lead);
                    left += v.l;
                    right += v.r;
                }
            }
            return match self.channels {
                Channels::Mono => Frame::mono(left),
                Channels::Stereo => Frame::new(left, right),
            };
        }
        let mut out = Frame::SILENT;
        for layer in &self.layers {
            if layer.muted {
                continue;
            }
            let (body, lead, to_seam) =
                layer.parts(i, len, fade, self.edge, self.pre_of(layer.pre_slot));
            let v = match part {
                Part::Steady => body.added(lead),
                Part::Gain(g) => scaled(body.added(lead), g),
                Part::Before(e) if to_seam == e - pos => body,
                Part::Before(e) => scaled(body.added(lead), fade_out(pos + fade - e, fade)),
                Part::After(e) if to_seam == e - pos => lead,
                Part::After(e) => scaled(body.added(lead), fade_in(pos + fade - e, fade)),
            };
            out = out.added(v);
        }
        match self.channels {
            Channels::Mono => Frame::mono(out.l),
            Channels::Stereo => out,
        }
    }

    /// What layer `layer` plays at loop index `idx` once the loop is heard steadily: its content,
    /// crossfaded into its pre-roll over the last fade frames before its seam. For tests that
    /// compare playback with the recorded layers.
    #[cfg(test)]
    pub fn steady_layer(&self, layer: usize, idx: u64) -> Frame {
        let (body, lead) = self.layer_parts(layer, idx);
        self.fan(body.added(lead))
    }

    /// Only the lead-in part of the same: what a layer whose own pass is still being recorded
    /// already adds before its seam.
    #[cfg(test)]
    pub fn lead_layer(&self, layer: usize, idx: u64) -> Frame {
        let (_, lead) = self.layer_parts(layer, idx);
        self.fan(lead)
    }

    #[cfg(test)]
    fn layer_parts(&self, layer: usize, idx: u64) -> (Frame, Frame) {
        match self.layers.get(layer) {
            Some(l) if !l.muted => {
                let pre = self.pre_of(l.pre_slot);
                let (body, lead, _) = l.parts(idx, self.grid.loop_len, self.fade_len(), self.edge, pre);
                (body, lead)
            }
            _ => (Frame::SILENT, Frame::SILENT),
        }
    }

    #[cfg(test)]
    fn fan(&self, f: Frame) -> Frame {
        match self.channels {
            Channels::Mono => Frame::mono(f.l),
            Channels::Stereo => f,
        }
    }

    /// A free pre-roll slot for a new layer, zeroed - in the audio thread, but bounded: one fade
    /// of samples. There is always one, since a track never holds more than `MAX_LAYERS` layers.
    fn claim_pre(&mut self) -> usize {
        let slot = (self.pre_free.trailing_zeros() as usize).min(MAX_LAYERS - 1);
        self.pre_free &= !(1 << slot);
        let base = slot * PRE_SAMPLES;
        self.pre_rolls[base..base + 2 * self.edge as usize].fill(0.0);
        slot
    }

    #[inline(always)]
    fn pre_of(&self, slot: usize) -> &[f32] {
        let base = slot * PRE_SAMPLES;
        &self.pre_rolls[base..base + PRE_SAMPLES]
    }

    /// Remember one input frame at musical position `pos` - every frame this track could record,
    /// whether a take runs or not, so a take that begins finds what led into it.
    #[inline(always)]
    pub fn remember(&mut self, pos: u64, frame: Frame) {
        if pos != self.history_end {
            // A jump (the first frame, a changed compensation): start a new run. The slot of a
            // position is `pos % edge`; in a run it simply counts up.
            self.history_from = pos;
            self.history_slot = (pos % self.edge) as usize;
        }
        let slot = self.history_slot * 2;
        self.history[slot] = frame.l;
        self.history[slot + 1] = frame.r;
        self.history_slot += 1;
        if self.history_slot as u64 == self.edge {
            self.history_slot = 0;
        }
        self.history_end = pos + 1;
    }

    /// The start of a take that has just written its first frame, once.
    #[inline]
    pub fn take_fresh_take(&mut self) -> Option<u64> {
        self.fresh_take.take()
    }

    /// The pre-roll of the newest layer, `k` frames into it (`0..edge`), for the recorder.
    #[inline]
    pub fn pre_roll(&self, layer: usize, k: usize) -> Frame {
        match self.layers.get(layer) {
            Some(l) => {
                let pre = self.pre_of(l.pre_slot);
                Frame::new(pre[2 * k], pre[2 * k + 1])
            }
            None => Frame::SILENT,
        }
    }

    /// Whether the transport stands; it fades the loop like a stop or a start that acts now. What
    /// was armed for later belongs to the run the transport change ends.
    pub fn hold(&mut self, held: bool, now: u64) {
        self.settle_play(u64::MAX);
        self.held = held;
        self.arm_switch(now, self.playing && !held, now);
    }

    /// Monitoring as heard at input index `k`: 1 while on, 0 while off, a fade over `edge` input
    /// frames after a switch.
    #[inline(always)]
    pub fn monitor_level(&self, k: u64) -> f32 {
        if self.monitor_from == self.monitor_to || k >= self.monitor_k0 + self.edge {
            return self.monitor_to;
        }
        if k < self.monitor_k0 {
            return self.monitor_from;
        }
        self.monitor_from + (self.monitor_to - self.monitor_from) * fade_in(k - self.monitor_k0, self.edge)
    }

    /// Whether monitoring contributes anything right now: on, or still fading out.
    #[inline(always)]
    pub fn monitor_active(&self) -> bool {
        self.monitor || self.monitor_from != self.monitor_to
    }

    /// Switch monitoring with a fade that begins at input index `k`.
    pub fn fade_monitor(&mut self, on: bool, k: u64) {
        self.fade_monitor_from(on, k, 1.0);
    }

    /// The same, when what is heard right now is only `reached` of this track's own level - a
    /// longer configured fade (`Command::MonitorFade`) that this switch ends: the ramp starts where
    /// that fade had got to.
    pub fn fade_monitor_from(&mut self, on: bool, k: u64, reached: f32) {
        if self.is_instrument() || (on == self.monitor && reached >= 1.0) {
            return;
        }
        let from = self.monitor_level(k) * reached;
        self.monitor = on;
        self.monitor_from = from;
        self.monitor_to = if on { 1.0 } else { 0.0 };
        self.monitor_k0 = k;
    }

    /// Arm a re-anchoring at musical position `at`: from there on the loop plays from its top, on
    /// every axis - read, write and the MIDI playback - each when it reaches `at`. `play` says the
    /// track is started there too (a `StartPlay` for `at` follows), so the read-ahead may sound
    /// from `at` on rather than from the moment the command acts.
    ///
    /// Refused (returns `false`) without a loop, and while the take that defines the loop is still
    /// open with no end or an end after `at` - its length is not known yet, and moving its top
    /// would tear it. An overdub may run up to `at`: it keeps the old grid for every frame before.
    /// Bounded work, nothing allocated.
    ///
    /// `now` is the read position about to be rendered: armed one fade length ahead, the old grid
    /// fades out and the new one fades in over the frames before `at` (see [`Track::play_frame`]).
    pub fn arm_restart(&mut self, at: u64, play: bool, now: u64) -> bool {
        if self.layers.is_empty() || self.grid.loop_len == 0 {
            return false;
        }
        if let Some(take) = self.grid.take
            && take.defines_loop
            && take.end.is_none_or(|end| end > at)
        {
            return false;
        }
        self.grid.arm_restart(at);
        self.restart_play = play;
        let fade = self.fade_len();
        let (w0, w1) = if at >= now + fade {
            (at - fade, at)
        } else if at > now {
            (now, at)
        } else {
            (now, now + fade)
        };
        self.restart_fade = Some(RestartFade { at, w0, w1 });
        if play {
            let on = !self.held;
            self.arm_switch(at, on, now);
        }
        true
    }

    /// An armed re-anchoring and whether it starts the track, for the MIDI playback.
    #[inline]
    pub fn restart(&self) -> Option<(u64, bool)> {
        self.grid.restart.map(|at| (at, self.restart_play))
    }

    /// Whether the transport stands. A held track keeps its layers and its play state but is not
    /// heard - its monitored input still is.
    /// At once, without a fade - for a track that is silent anyway. See [`Track::hold`].
    #[inline]
    pub fn set_held(&mut self, held: bool) {
        self.held = held;
        self.sound_base = self.playing && !held;
        self.switch_count = 0;
    }

    #[inline]
    pub fn held(&self) -> bool {
        self.held
    }

    /// Re-anchor the loop so its top sits at `origin`. Used when the transport starts again: the
    /// loop then plays from its beginning on the new bar 1. Only meaningful without a running take,
    /// which the caller makes sure of.
    pub fn rebase(&mut self, origin: u64) {
        if !self.layers.is_empty() {
            self.grid.origin = origin;
        }
        // A re-anchoring armed for later belongs to the run that has just been replaced.
        self.grid.restart = None;
        self.restart_play = false;
        self.restart_fade = None;
    }

    #[inline]
    pub fn monitor(&self) -> bool {
        self.monitor
    }

    /// Switch monitoring. An instrument track ignores it: it is heard whenever it plays, unless it
    /// was set up with monitoring off - a score that only monitors a track while it records would
    /// otherwise silence the piano between two takes.
    #[inline]
    pub fn set_monitor(&mut self, on: bool) {
        if !self.is_instrument() {
            self.monitor = on;
            let level = if on { 1.0 } else { 0.0 };
            self.monitor_from = level;
            self.monitor_to = level;
        }
    }

    #[inline]
    pub fn take(&self) -> Option<Take> {
        self.grid.take
    }

    /// The geometry of this track: origin, loop length, take state.
    #[inline]
    pub fn grid(&self) -> &LoopGrid {
        &self.grid
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
        match self.grid.take {
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
        let origin = self.grid.origin_at(pos);
        if self.grid.loop_len == 0 || pos < origin || self.midi_loop {
            return Frame::SILENT;
        }
        let frame_index = ((pos - origin) % self.grid.loop_len) as usize;
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
            let samples = layer.samples();
            if let Some(&v) = samples.get(base) {
                left += v * layer.gain;
            }
            if n == 2 {
                if let Some(&v) = samples.get(base + 1) {
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
        let Some(take) = self.grid.take else {
            return;
        };
        let channels = self.channels;
        let Some(layer) = self.layers.get_mut(take.layer) else {
            return;
        };
        let capacity = layer.frames();
        let LayerStore::Audio(buffer) = &mut layer.store else {
            return;
        };
        let Some(index) = self.grid.write_index(pos, capacity) else {
            return;
        };
        let n = channels.count();
        // The second and last place a frame index becomes a buffer offset.
        let base = index as usize * n;
        for c in 0..n {
            if let Some(slot) = buffer.get_mut(base + c) {
                *slot = frame.channel(c);
            }
        }
        if !layer.pre_ready {
            // The take's first frame: where it sits on the grid is its seam, and what arrived in
            // the `edge` frames before it is its pre-roll. Bounded: `edge` frames copied, once.
            layer.pre_ready = true;
            layer.seam = index;
            layer.end = index;
            let edge = self.edge;
            for k in 0..edge {
                let p = pos + k;
                let Some(p) = p.checked_sub(edge) else {
                    continue;
                };
                if p >= self.history_from && p < self.history_end {
                    let slot = (p % edge) as usize * 2;
                    let at = layer.pre_slot * PRE_SAMPLES + 2 * k as usize;
                    self.pre_rolls[at] = self.history[slot];
                    self.pre_rolls[at + 1] = self.history[slot + 1];
                }
            }
            self.fresh_take = Some(pos);
        }
        layer.written += 1;
        layer.last = index;
    }

    /// Musical position at which the running take has to end at the latest, if any.
    ///
    /// A loop-defining take is bounded by the buffer; an overdub is bounded by one pass through the
    /// loop, so it can never overwrite what it has just recorded. Both in frames.
    #[inline]
    pub fn take_limit(&self) -> Option<u64> {
        let take = self.grid.take?;
        let layer = self.layers.get(take.layer)?;
        self.grid.take_limit(layer.frames())
    }

    /// Start the first take of a new loop. The caller has already returned the old layers.
    ///
    /// An audio buffer has to be long enough for `frames * channels` samples; the caller
    /// (`process::take_spare`) picks it from the pool of the right channel count.
    pub fn begin_loop_take(&mut self, start: u64, store: impl Into<LayerStore>) {
        let store = store.into();
        self.check_store(&store);
        self.set_playing(false);
        let slot = self.claim_pre();
        self.layers.push(Layer::new(store, self.channels, slot));
        self.grid.begin_loop(start, self.layers.len() - 1);
    }

    /// Start a further layer on the existing grid.
    pub fn begin_overdub_take(&mut self, start: u64, store: impl Into<LayerStore>) {
        let store = store.into();
        self.check_store(&store);
        let slot = self.claim_pre();
        self.layers.push(Layer::new(store, self.channels, slot));
        self.grid.begin_overdub(start, self.layers.len() - 1);
    }

    #[inline]
    fn check_store(&self, store: &LayerStore) {
        debug_assert!(self.layers.len() < MAX_LAYERS);
        if let LayerStore::Audio(buffer) = store {
            debug_assert_eq!(
                buffer.len() % self.channels.count(),
                0,
                "Puffer passt nicht zur Kanalzahl des Tracks"
            );
        }
    }

    /// Is a take running whose end is already known, i.e. one that is only waiting for its last
    /// `R` samples to arrive? A further take may be scheduled behind it.
    #[inline]
    pub fn take_is_finishing(&self) -> bool {
        self.grid.take_is_finishing()
    }

    #[inline]
    pub fn has_pending(&self) -> bool {
        self.grid.pending.is_some()
    }

    #[inline]
        pub fn pending_defines_loop(&self) -> bool {
        self.grid.pending.map(|p| p.defines_loop).unwrap_or(false)
    }

    /// Queue a take behind the running one. Returns the storage of a pending take that was
    /// replaced.
    pub fn set_pending(
        &mut self,
        start: u64,
        defines_loop: bool,
        store: impl Into<LayerStore>,
    ) -> Option<LayerStore> {
        let previous = self.pending_store.replace(store.into());
        self.grid.pending = Some(PendingGeom {
            start,
            end: None,
            defines_loop,
        });
        previous
    }

    /// Start the queued take now that the previous one is closed. Returns its storage instead when
    /// it cannot be started after all.
    pub fn promote_pending(&mut self) -> Option<LayerStore> {
        let pending = self.grid.pending.take()?;
        let store = self.pending_store.take()?;
        if self.layers.len() >= MAX_LAYERS {
            return Some(store);
        }
        if pending.defines_loop {
            self.begin_loop_take(pending.start, store);
        } else {
            self.begin_overdub_take(pending.start, store);
        }
        if let Some(end) = pending.end {
            self.set_take_end(end);
        }
        None
    }

    /// Hand back the storage of a queued take, cancelling it.
    pub fn take_pending_buffer(&mut self) -> Option<LayerStore> {
        self.grid.pending = None;
        self.pending_store.take()
    }

    /// Announce where the running take ends, before the last sample has been written.
    ///
    /// Needed because the write pointer trails the play pointer by the compensated latency: when
    /// the output reaches the end of the take, the last `R` samples are still on their way in.
    /// Publishing the length early is what lets playback start seamlessly at the loop boundary;
    /// the zeroed buffer keeps the not-yet-written tail silent instead of stale.
    pub fn set_take_end(&mut self, end: u64) {
        let capacity = self.take_capacity();
        self.grid.set_take_end(end, capacity);
    }

    /// Close the take. `end` is the musical position just after its last sample.
    pub fn finish_take(&mut self, end: u64) {
        let Some(take) = self.grid.finish_take(end) else {
            return;
        };
        let len = self.grid.loop_len;
        if let Some(layer) = self.layers.get_mut(take.layer)
            && len > 0
        {
            if take.defines_loop {
                layer.seam = 0;
                layer.end = 0;
            } else if layer.written < len {
                // Cut short: the layer ends inside the loop and fades out there.
                layer.end = (layer.last + 1) % len;
            } else {
                layer.end = layer.seam;
            }
        }
    }

    /// Frames the running take's storage can hold.
    fn take_capacity(&self) -> u64 {
        self.grid
            .take
            .and_then(|t| self.layers.get(t.layer))
            .map(Layer::frames)
            .unwrap_or(0)
    }

    /// Hand back the last layer, so the caller can return its storage to the control thread.
    ///
    /// Emptying a track resets its geometry: the next take defines a new loop.
    pub fn pop_layer(&mut self) -> Option<LayerStore> {
        let layer = self.layers.pop()?;
        self.pre_free |= 1 << layer.pre_slot;
        if self.layers.is_empty() {
            self.grid.reset();
            self.set_playing(false);
        }
        Some(layer.store)
    }

    /// Remove one layer by index, handing its storage back. Later layers move down by one.
    pub fn remove_layer(&mut self, index: usize) -> Option<LayerStore> {
        if index >= self.layers.len() {
            return None;
        }
        // Moves at most `MAX_LAYERS` small headers - a memmove of a few hundred bytes, no
        // allocation and no deallocation.
        let layer = self.layers.remove(index);
        self.pre_free |= 1 << layer.pre_slot;
        if self.layers.is_empty() {
            self.grid.reset();
            self.set_playing(false);
        }
        Some(layer.store)
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

    pub fn layer(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    pub fn layer_mut(&mut self, index: usize) -> Option<&mut Layer> {
        self.layers.get_mut(index)
    }

    /// Every layer, for the MIDI playback that walks them all.
    pub fn layers_mut(&mut self) -> &mut [Layer] {
        &mut self.layers
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Cancel a running take without touching the layers that were finished before it.
    pub fn cancel_take(&mut self) {
        self.grid.take = None;
    }

    /// A MIDI take covers time rather than samples: the engine tells a loop-defining one how far the
    /// musical position has come, which is what bounds its loop length when it closes.
    pub fn advance_filled(&mut self, pos: u64) {
        let capacity = self.take_capacity();
        if let Some(take) = self.grid.take
            && take.defines_loop
            && pos >= self.grid.origin
        {
            let covered = (pos - self.grid.origin).min(capacity);
            self.grid.filled = self.grid.filled.max(covered);
        }
    }

    /// Whether this track's takes store notes instead of sound - a MIDI loop.
    #[inline(always)]
    pub fn midi_loop(&self) -> bool {
        self.midi_loop
    }

    /// Switch what this track's takes store. Only on an instrument track, and only while it holds
    /// nothing and records nothing - layers of two kinds on one track are not a thing yet.
    pub fn set_midi_loop(&mut self, on: bool) -> bool {
        if on && !self.is_instrument() {
            return false;
        }
        if on == self.midi_loop {
            return true;
        }
        if !self.layers.is_empty() || self.grid.take.is_some() || self.grid.pending.is_some() {
            return false;
        }
        self.midi_loop = on;
        true
    }

    /// Built that way by the control thread.
    #[must_use]
    pub fn with_midi_loop(mut self, on: bool) -> Self {
        self.set_midi_loop(on);
        self
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
        let level = if self.monitor { 1.0 } else { 0.0 };
        self.monitor_from = level;
        self.monitor_to = level;
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
        // The channel fader, mute and solo are left where they are. They are the mix the musician
        // is riding *now*, set by ear on the mixer rather than written into a setup, and "clear
        // all" means "record the song again", not "throw away the balance". There is no start
        // value to go back to either - unlike the pan, no configuration names a fader.
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

    /// A loop of 100 frames whose index `i` holds `i + 1`, so every index is recognisable.
    fn counting_loop() -> Track {
        let mut t = track_with_layer(100);
        for i in 0..100u64 {
            t.write(1_000 + i, Frame::mono(i as f32 + 1.0));
        }
        t.set_take_end(1_100);
        t.finish_take(1_100);
        t
    }

    /// The loop is read ahead for the latency of its chain and plugins. A loop that comes back with
    /// a re-anchoring is heard from its very first frame: the read-ahead reaches the new top before
    /// the output does, and from there the track sounds on the new grid - before it, nothing.
    #[test]
    fn a_re_anchored_loop_read_ahead_starts_with_its_first_frame() {
        let mut t = counting_loop();
        let advance = 30u64;
        t.set_playback_advance(advance);
        let at = 5_037u64;
        assert!(t.arm_restart(at, true, 0));
        // Output position `at - advance` feeds the chain with the loop's index 0.
        let (before, _) = t.render_unpanned(at - advance - 1, Frame::SILENT, false);
        assert_eq!(before[0], Frame::SILENT, "noch still");
        let (first, _) = t.render_unpanned(at - advance, Frame::SILENT, false);
        assert_eq!(first[0], Frame::mono(1.0), "Index 0 zuerst");
        assert_eq!(t.read(at + 99), Frame::mono(100.0));
        assert_eq!(t.read(at + 100), Frame::mono(1.0));
        // Before the re-anchoring the old grid holds: 5_036 - 1_000 = 40 * 100 + 36.
        assert_eq!(t.read(at - 1), Frame::mono(37.0));
        assert_eq!(t.origin(), at, "der Status zeigt das neue Raster");
        // A stop takes the pending start back.
        t.set_playing(false);
        let (after_stop, _) = t.render_unpanned(at, Frame::SILENT, false);
        assert_eq!(after_stop[0], Frame::SILENT);
    }

    /// Refused while the take that defines the loop is still open past the re-anchoring - its
    /// length is not known yet - and on an empty track.
    #[test]
    fn a_re_anchoring_needs_a_loop_whose_length_is_known() {
        let mut empty = Track::mono(0, false, 48_000);
        assert!(!empty.arm_restart(10, true, 0));
        let mut t = track_with_layer(100);
        t.write(1_000, Frame::mono(1.0));
        assert!(!t.arm_restart(2_000, true, 0), "Take offen");
        t.set_take_end(1_050);
        assert!(!t.arm_restart(1_040, true, 0), "Re-Anker mitten im Take");
        assert!(t.arm_restart(1_050, true, 0), "Take endet vorher, wartet nur auf seine letzten Frames");
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
