//! The effect chain of one track: high-pass, three-band EQ, compressor, delay, reverb.
//!
//! ```text
//!   Ebenen-Summe  ─┐
//!                  ├─► auf stereo ─► Hochpass ─► EQ ─► Kompressor ─► Delay ─► Hall ─► Panorama
//!   Mithoer-Signal ┘   (nur mono)                                                       │
//!                                                                                       ▼
//!   Eingangssignal ────────────────────────────────────────────► Loop-Puffer  (trocken)  Mixbus
//! ```
//!
//! # The chain is stereo, whatever the track is
//!
//! A loop buffer is mono or stereo depending on its source (see [`super::frame`]), but everything
//! from here on is stereo without exception. A mono track is fanned out to two channels before it
//! reaches the chain, and the reason is the reverb: a hall on a mono guitar has to be a stereo
//! hall, or the guitar stays a point in the middle of the head no matter how big the room is set
//! to. The same holds for the delay, whose repeats keep the side they came from.
//!
//! What that costs is one extra multiply-add per sample in the filters, two delay lines instead of
//! one, and two comb banks instead of one - measured in [`bench`], and small enough that eight
//! tracks still fit into a fraction of the callback budget.
//!
//! The two effects that would be *wrong* to run twice do not: the compressor has one detector and
//! one gain for both channels (`dynamics`), and the reverb excites both of its banks with the same
//! signal (`reverb`). Both decisions are argued where the code is.
//!
//! # The one rule that decides everything else: recording stays dry
//!
//! The chain sits in the *playback* path and nowhere else. What goes into a layer buffer is the
//! input sample, latency-compensated and otherwise untouched, exactly as before this module
//! existed. `engine::process::record_input` reads the raw input slice and never sees a chain.
//!
//! The reason is not purity, it is that a decision baked into a recording cannot be taken back. A
//! compressor set too hard on the take that turns out to be the good one is a lost take. Effects
//! on playback can be changed, switched off and re-dialled between two passes of the same loop.
//!
//! # Why monitoring goes through the same chain
//!
//! A singer with reverb on the monitor sings differently from a singer without it - quieter,
//! with less push, and in tune with what he hears. So the live signal a track monitors is summed
//! with that track's played-back layers *before* the chain, and both go through it together. One
//! chain, one instance, one state: whatever the loop sounds like, the live voice over it sounds
//! the same. Two parallel chains would drift apart the moment a parameter changed, and a reverb
//! fed twice would be twice as loud.
//!
//! This is the reason the chain lives on the [`Track`](super::track::Track) and not next to the
//! output mix.
//!
//! # Real-time behaviour
//!
//! * **Nothing here allocates after construction.** The two delay lines and the twenty-four reverb
//!   buffers are `Vec`s created in [`Chain::new`], which the control thread calls while building a
//!   `Track`, exactly the way layer buffers are made. Inside the callback nothing grows, shrinks
//!   or is dropped.
//! * **Nothing here locks or logs.** Parameter changes arrive as `Copy` commands through the
//!   existing lock-free queue.
//! * Coefficient arithmetic (`sin`, `cos`, `powf`) happens when a parameter changes, i.e. a few
//!   dozen operations per command, never per sample.
//!
//! # Denormals
//!
//! Delay and reverb are feedback loops that keep multiplying their contents by something under
//! 1.0 forever after the music stops. Left alone they spend minutes in the denormal range, where a
//! multiply costs one to two orders of magnitude more than it should - so a *silent* looper would
//! be the most expensive state the looper has. Two measures, and both are needed:
//!
//! 1. Every feedback store goes through `biquad::flush`, which turns anything below 1e-30 into a
//!    true zero. That empties the buffers instead of letting them idle at 1e-40.
//! 2. The chain counts how long its input has been exactly zero and stops processing altogether
//!    once that exceeds the longest tail the current settings can produce ([`Chain::tail_samples`]).
//!    An idle track then costs one comparison per sample instead of a full chain.
//!
//! # Bypass
//!
//! A bypassed chain returns its input bit for bit - not "almost", the same float. That is what
//! makes it usable as a panic switch and what the test `bypass_is_bit_identical` pins down. The
//! switch itself is still a crossfade so it does not click; the pass-through becomes bit-exact
//! once the crossfade has reached exactly zero, which [`smooth::Smoothed`] guarantees.

pub mod bench;
pub mod biquad;
pub mod delay;
pub mod dynamics;
pub mod reverb;
pub mod smooth;

pub use biquad::BandKind;
pub use delay::DelayNote;
pub use dynamics::CompSettings;

use super::frame::Frame;
use biquad::{Biquad, Coeffs};
use delay::Delay;
use dynamics::Compressor;
use reverb::Reverb;
use smooth::Smoothed;

/// Number of switchable effects in a chain.
pub const FX_SLOTS: usize = 5;
/// Number of EQ bands.
pub const EQ_BANDS: usize = 3;

/// Crossfade time when a filter or the compressor is switched. Short: these have no tail, so
/// there is nothing to fade out, only a step to avoid.
const SWITCH_MS: f32 = 20.0;
/// Crossfade time when delay or reverb is switched. Longer, because switching one off cuts a tail
/// and a 20 ms cut of a reverb tail sounds like a door closing.
const TAIL_SWITCH_MS: f32 = 80.0;
/// Crossfade time of the whole-chain bypass.
const BYPASS_MS: f32 = 25.0;
/// Safety margin added to the computed tail before a silent chain is allowed to go idle.
const TAIL_MARGIN_MS: f32 = 500.0;

// ---------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------

/// One switchable position in the chain. The order of the variants **is** the order of the signal
/// path, and the numbering the user sees is this order plus one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxSlot {
    HighPass,
    Eq,
    Comp,
    Delay,
    Reverb,
}

impl FxSlot {
    #[inline]
    pub fn index(self) -> usize {
        match self {
            FxSlot::HighPass => 0,
            FxSlot::Eq => 1,
            FxSlot::Comp => 2,
            FxSlot::Delay => 3,
            FxSlot::Reverb => 4,
        }
    }

    pub fn all() -> [FxSlot; FX_SLOTS] {
        [
            FxSlot::HighPass,
            FxSlot::Eq,
            FxSlot::Comp,
            FxSlot::Delay,
            FxSlot::Reverb,
        ]
    }

    /// German name, for the display.
    pub fn label(self) -> &'static str {
        match self {
            FxSlot::HighPass => "Hochpass",
            FxSlot::Eq => "EQ",
            FxSlot::Comp => "Kompressor",
            FxSlot::Delay => "Delay",
            FxSlot::Reverb => "Hall",
        }
    }

    /// Machine name, as a command and a MIDI address spell it. The app's wire format
    /// (`FxSlotName`) uses exactly these words, and so does `midi::Target`.
    pub fn name(self) -> &'static str {
        match self {
            FxSlot::HighPass => "high_pass",
            FxSlot::Eq => "eq",
            FxSlot::Comp => "comp",
            FxSlot::Delay => "delay",
            FxSlot::Reverb => "reverb",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::all().into_iter().find(|slot| slot.name() == text)
    }

    /// One letter for the compact status line: `HEKDR`.
    pub fn letter(self) -> char {
        match self {
            FxSlot::HighPass => 'H',
            FxSlot::Eq => 'E',
            FxSlot::Comp => 'K',
            FxSlot::Delay => 'D',
            FxSlot::Reverb => 'R',
        }
    }

    /// One-based number as typed in the CLI.
    pub fn from_number(n: usize) -> Option<Self> {
        Self::all().get(n.checked_sub(1)?).copied()
    }
}

/// The ready-made chains. On stage nobody turns knobs - the musician picks a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FxPreset {
    /// Everything off, chain bypassed. The state a track starts in, so nothing this module does
    /// can change how the looper behaved before it existed.
    #[default]
    Dry,
    Voice,
    PiezoGuitar,
    /// Not loadable - reported when a parameter has been changed by hand since the last preset.
    Custom,
}

impl FxPreset {
    pub fn label(self) -> &'static str {
        match self {
            FxPreset::Dry => "trocken",
            FxPreset::Voice => "stimme",
            FxPreset::PiezoGuitar => "gitarre",
            FxPreset::Custom => "eigen",
        }
    }

    /// The three a user can ask for. `Custom` is a report, not a choice.
    pub fn loadable() -> [FxPreset; 3] {
        [FxPreset::Dry, FxPreset::Voice, FxPreset::PiezoGuitar]
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "trocken" | "dry" | "aus" => Some(FxPreset::Dry),
            "stimme" | "voice" | "gesang" => Some(FxPreset::Voice),
            "gitarre" | "piezo" | "guitar" => Some(FxPreset::PiezoGuitar),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

/// One EQ band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandSettings {
    pub kind: BandKind,
    pub hz: f32,
    pub q: f32,
    pub gain_db: f32,
}

impl Default for BandSettings {
    fn default() -> Self {
        Self {
            kind: BandKind::Peak,
            hz: 1_000.0,
            q: 1.0,
            gain_db: 0.0,
        }
    }
}

impl BandSettings {
    fn clamped(self) -> Self {
        Self {
            kind: self.kind,
            hz: self.hz.clamp(20.0, 20_000.0),
            q: self.q.clamp(0.1, 12.0),
            gain_db: self.gain_db.clamp(-24.0, 24.0),
        }
    }

    /// A band that does nothing costs nothing: the chain skips it entirely.
    #[inline]
    fn is_flat(&self) -> bool {
        self.gain_db == 0.0
    }
}

/// Every knob of a chain, as plain data. This is what a preset *is*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChainSettings {
    pub enabled: [bool; FX_SLOTS],
    pub high_pass_hz: f32,
    pub bands: [BandSettings; EQ_BANDS],
    pub comp: CompSettings,
    pub delay_note: DelayNote,
    pub delay_feedback: f32,
    pub delay_mix: f32,
    pub reverb_size: f32,
    pub reverb_damping: f32,
    pub reverb_mix: f32,
}

impl Default for ChainSettings {
    fn default() -> Self {
        Self {
            enabled: [false; FX_SLOTS],
            high_pass_hz: 80.0,
            bands: [BandSettings::default(); EQ_BANDS],
            comp: CompSettings::default(),
            delay_note: DelayNote::DottedEighth,
            delay_feedback: 0.3,
            delay_mix: 0.0,
            reverb_size: 0.6,
            reverb_damping: 0.5,
            reverb_mix: 0.0,
        }
    }
}

impl FxPreset {
    /// The values behind a name.
    ///
    /// Every number below is a judgement call, so every number below says why it is what it is.
    /// Whoever changes one should be able to argue against the sentence next to it.
    pub fn settings(self) -> ChainSettings {
        match self {
            // -------------------------------------------------------------------------------
            FxPreset::Dry | FxPreset::Custom => ChainSettings::default(),

            // -------------------------------------------------------------------------------
            // Voice. This is the "Schmackes" the musician asked for: a high-pass that takes the
            // rumble out, a small presence lift that puts the voice in front of the guitar,
            // compression that levels a singer-songwriter's dynamics, and enough reverb that the
            // voice stops sounding like a close-miked demo.
            FxPreset::Voice => ChainSettings {
                enabled: [true, true, true, false, true],
                // 80 Hz: under the lowest fundamental a sung male voice produces (about 85 Hz)
                // and far under a female one, so nothing of the voice itself is touched. What
                // goes is handling noise, the thump of a plosive and whatever the stage floor
                // sends up the mic stand.
                high_pass_hz: 80.0,
                bands: [
                    // 300 Hz down 2 dB: the "boxy" region of a close-miked voice, which the
                    // proximity effect of a cardioid piles up. A gentle cut, not a hole - too
                    // much here and the voice loses its body.
                    BandSettings {
                        kind: BandKind::Peak,
                        hz: 300.0,
                        q: 1.0,
                        gain_db: -2.0,
                    },
                    // 3 kHz up 2.5 dB: consonants and the range the ear is most sensitive in.
                    // This is what makes the words intelligible over a strummed guitar, and the
                    // single most useful move on a voice. Broad (Q 0.9), because a narrow
                    // presence lift sounds like a telephone.
                    BandSettings {
                        kind: BandKind::Peak,
                        hz: 3_000.0,
                        q: 0.9,
                        gain_db: 2.5,
                    },
                    // 8 kHz shelf up 2 dB: air. A shelf and not a bell, because "air" is the
                    // whole top end, not one frequency - see BandKind::HighShelf.
                    BandSettings {
                        kind: BandKind::HighShelf,
                        hz: 8_000.0,
                        q: 0.707,
                        gain_db: 2.0,
                    },
                ],
                comp: CompSettings {
                    // -18 dBFS: a voice sung into the Scarlett at a sensible gain peaks around
                    // -10 dBFS, so this catches the loud half of a phrase and leaves the quiet
                    // half alone.
                    threshold_db: -18.0,
                    // 3:1 is levelling, not limiting. Above about 6:1 a voice starts to sound
                    // like it is being held down, which is the opposite of what is wanted here.
                    ratio: 3.0,
                    // 12 ms lets the transient of a consonant through before the gain moves, so
                    // "t" and "k" keep their edge. A 1 ms attack on a voice eats exactly those
                    // and the result sounds muffled and loud at the same time.
                    attack_ms: 12.0,
                    // 180 ms is roughly one syllable: fast enough to recover between words, slow
                    // enough that it does not pump with the vibrato.
                    release_ms: 180.0,
                    // 8 dB knee - see the module comment of `dynamics`: a singer lives *at* the
                    // threshold, and a hard knee would toggle several times a second.
                    knee_db: 8.0,
                    // At -18 dB threshold and 3:1 a typical phrase loses about 5 to 6 dB, so
                    // +6 dB puts the level back where it was. Switching the preset on should
                    // make the voice denser, not louder - that is what "Schmackes" means.
                    makeup_db: 6.0,
                },
                delay_note: DelayNote::DottedEighth,
                delay_feedback: 0.3,
                delay_mix: 0.22,
                // 0.62 is a small hall, about 1.5 seconds. Damping 0.45 takes the top off the
                // tail so the reverb does not add sibilance. 18 % wet is the level at which a
                // listener notices that something is missing when you switch it off, but cannot
                // say what it is - which is the right amount for a song, not for a demo of a
                // reverb.
                reverb_size: 0.62,
                reverb_damping: 0.45,
                reverb_mix: 0.18,
            },

            // -------------------------------------------------------------------------------
            // Piezo guitar. A piezo under the saddle hears the top of the guitar, not the air in
            // front of it: too much low-mid boom, and a hard, glassy peak in the upper mids that
            // everybody calls "quack". Both of those are what this preset is for.
            FxPreset::PiezoGuitar => ChainSettings {
                enabled: [true, true, true, false, true],
                // 100 Hz, higher than the voice on purpose. Honest caveat: the low E of a guitar
                // in standard tuning is 82 Hz, so a 12 dB/oct high-pass at 100 Hz does take about
                // 5 dB off that fundamental. That is deliberate - a piezo reproduces almost none
                // of the low E's fundamental anyway, while everything the pickup adds down there
                // is body boom and stage rumble. For a dropped tuning this belongs at 80 Hz.
                high_pass_hz: 100.0,
                bands: [
                    // 180 Hz down 3 dB: the main body resonance of a dreadnought seen through a
                    // piezo, i.e. the boom that makes an acoustic guitar disappear in a mix.
                    BandSettings {
                        kind: BandKind::Peak,
                        hz: 180.0,
                        q: 1.2,
                        gain_db: -3.0,
                    },
                    // 3 kHz down 4 dB, Q 1.4: the quack. The brief names 2 to 4 kHz; 3 kHz is the
                    // middle of that and a Q of 1.4 covers roughly 2.2 to 4.2 kHz, so the whole
                    // named range is inside the cut and nothing outside it is.
                    BandSettings {
                        kind: BandKind::Peak,
                        hz: 3_000.0,
                        q: 1.4,
                        gain_db: -4.0,
                    },
                    // 6 kHz shelf up 1.5 dB: the cut above took the string noise with it, and a
                    // steel string without its sparkle sounds like a nylon one. A small shelf
                    // gives it back above the quack instead of inside it.
                    BandSettings {
                        kind: BandKind::HighShelf,
                        hz: 6_000.0,
                        q: 0.707,
                        gain_db: 1.5,
                    },
                ],
                comp: CompSettings {
                    // A strummed guitar is louder than a sung phrase, so the threshold sits
                    // higher; it should catch the hard strums, not the picked verse.
                    threshold_db: -14.0,
                    // "Leichte Kompression" - 2.5:1 evens the strumming hand out and no more.
                    ratio: 2.5,
                    // 25 ms, twice as slow as the voice. The pick attack *is* the sound of an
                    // acoustic guitar; a fast attack flattens it and the guitar starts to sound
                    // like a rubber band.
                    attack_ms: 25.0,
                    // 250 ms: about one strum at a moderate tempo, so the gain is back up before
                    // the next one.
                    release_ms: 250.0,
                    // Slightly narrower knee than the voice: a guitar's level is decided in
                    // discrete events by the picking hand, not continuously.
                    knee_db: 6.0,
                    // 2.5:1 from -14 dB costs about 3 dB on a strummed part.
                    makeup_db: 3.0,
                },
                delay_note: DelayNote::DottedEighth,
                delay_feedback: 0.25,
                delay_mix: 0.15,
                // "Wenig Hall": a smaller room than the voice and more damping, because a piezo
                // is already bright and a bright reverb on top of it turns into hiss. 10 % wet
                // puts the guitar in the same room as the voice without blurring the strum.
                reverb_size: 0.5,
                reverb_damping: 0.6,
                reverb_mix: 0.10,
            },
        }
    }

    /// Whether loading this preset also bypasses the whole chain. Only "trocken" does.
    pub fn bypasses(self) -> bool {
        matches!(self, FxPreset::Dry)
    }
}

// ---------------------------------------------------------------------------------------------
// Parameters as commands
// ---------------------------------------------------------------------------------------------

/// One parameter change. `Copy` and fixed size, so it travels in the existing command queue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FxParam {
    HighPassHz(f32),
    BandKind { band: usize, kind: BandKind },
    BandHz { band: usize, hz: f32 },
    BandQ { band: usize, q: f32 },
    BandGainDb { band: usize, db: f32 },
    CompThresholdDb(f32),
    CompRatio(f32),
    CompAttackMs(f32),
    CompReleaseMs(f32),
    CompKneeDb(f32),
    CompMakeupDb(f32),
    DelayNote(DelayNote),
    DelayFeedback(f32),
    DelayMix(f32),
    ReverbSize(f32),
    ReverbDamping(f32),
    ReverbMix(f32),
}

/// What the chain of one track currently is, for the status snapshot. `Copy`, like everything
/// that crosses the thread boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxStatus {
    pub bypass: bool,
    pub preset: FxPreset,
    pub settings: ChainSettings,
    /// Delay time actually sounding, in samples - the tempo-synchronous number.
    pub delay_samples: u64,
    /// Deepest gain reduction since the last snapshot, in dB and never positive.
    pub reduction_db: f32,
}

impl Default for FxStatus {
    fn default() -> Self {
        Self {
            bypass: true,
            preset: FxPreset::Dry,
            settings: ChainSettings::default(),
            delay_samples: 0,
            reduction_db: 0.0,
        }
    }
}

impl FxStatus {
    /// `HEK-R` style summary: the letter of every effect that is on, a dash for one that is off.
    /// Five characters, always, so the terminal display does not jump.
    pub fn letters(&self) -> String {
        FxSlot::all()
            .iter()
            .map(|slot| {
                if self.settings.enabled[slot.index()] {
                    slot.letter()
                } else {
                    '-'
                }
            })
            .collect()
    }

    /// True when the chain is doing something audible.
    pub fn active(&self) -> bool {
        !self.bypass && self.settings.enabled.iter().any(|&on| on)
    }
}

// ---------------------------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------------------------

pub struct Chain {
    sample_rate: u32,
    bypass: bool,
    /// 1.0 = chain audible, 0.0 = bit-exact pass-through.
    bypass_mix: Smoothed,
    /// Wet share of each slot; 0.0 means the slot is skipped entirely.
    slot_mix: [Smoothed; FX_SLOTS],
    /// One filter instance per channel. A biquad's state is four *signal* samples, so sharing one
    /// between two channels would feed each channel the other's history - which is not a filter of
    /// anything, it is a ring modulator with extra steps.
    hp: [Biquad; 2],
    bands: [[Biquad; 2]; EQ_BANDS],
    comp: Compressor,
    delay: Delay,
    reverb: Reverb,
    set: ChainSettings,
    preset: FxPreset,
    /// Consecutive samples of digital silence at the input.
    silence_run: u32,
    /// After this many silent samples nothing can come out any more and the chain goes idle.
    tail_samples: u32,
}

impl Chain {
    /// Builds every buffer the chain will ever use. **Control thread only** - this allocates
    /// roughly 440 kB per track (delay line plus reverb), the same way `Track::new` allocates its
    /// layer vector.
    pub fn new(sample_rate: u32) -> Self {
        let set = ChainSettings::default();
        let mut chain = Self {
            sample_rate,
            bypass: true,
            bypass_mix: Smoothed::new(0.0, BYPASS_MS, sample_rate),
            slot_mix: [Smoothed::new(0.0, SWITCH_MS, sample_rate); FX_SLOTS],
            hp: [Biquad::new(Coeffs::high_pass(
                sample_rate,
                set.high_pass_hz,
                std::f32::consts::FRAC_1_SQRT_2,
            )); 2],
            bands: [[Biquad::default(); 2]; EQ_BANDS],
            comp: Compressor::new(sample_rate, set.comp),
            delay: Delay::new(
                sample_rate,
                set.delay_note,
                set.delay_feedback,
                set.delay_mix,
            ),
            reverb: Reverb::new(
                sample_rate,
                set.reverb_size,
                set.reverb_damping,
                set.reverb_mix,
            ),
            set,
            preset: FxPreset::Dry,
            silence_run: u32::MAX,
            tail_samples: sample_rate,
        };
        // The two tails switch more slowly than the filters do - see TAIL_SWITCH_MS.
        chain.slot_mix[FxSlot::Delay.index()] = Smoothed::new(0.0, TAIL_SWITCH_MS, sample_rate);
        chain.slot_mix[FxSlot::Reverb.index()] = Smoothed::new(0.0, TAIL_SWITCH_MS, sample_rate);
        chain.rebuild_bands();
        chain.refresh_tail();
        chain
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn settings(&self) -> ChainSettings {
        self.set
    }

    pub fn preset(&self) -> FxPreset {
        self.preset
    }

    pub fn bypassed(&self) -> bool {
        self.bypass
    }

    pub fn enabled(&self, slot: FxSlot) -> bool {
        self.set.enabled[slot.index()]
    }

    /// Longest silence after which the chain may stop working. See the module comment.
    pub fn tail_samples(&self) -> u32 {
        self.tail_samples
    }

    pub fn status(&mut self) -> FxStatus {
        FxStatus {
            bypass: self.bypass,
            preset: self.preset,
            settings: self.set,
            delay_samples: self.delay.target_tap_samples(),
            reduction_db: self.comp.take_reduction_db(),
        }
    }

    /// What a quarter note is worth right now. Called once per audio block by the engine; the
    /// delay ignores it unless the value actually changed.
    #[inline]
    pub fn set_quarter_samples(&mut self, quarter_samples: f64) {
        let before = self.delay.target_tap_samples();
        self.delay.set_quarter_samples(quarter_samples);
        if self.delay.target_tap_samples() != before {
            self.refresh_tail();
        }
    }

    // ---- switching ---------------------------------------------------------------------------

    pub fn set_bypass(&mut self, on: bool) {
        self.bypass = on;
        self.bypass_mix.set(if on { 0.0 } else { 1.0 });
        if !on {
            // Coming back from bypass, the filters hold samples from before the switch.
            self.reset_states();
        }
    }

    pub fn set_enabled(&mut self, slot: FxSlot, on: bool) {
        let i = slot.index();
        if self.set.enabled[i] == on {
            return;
        }
        self.set.enabled[i] = on;
        self.slot_mix[i].set(if on { 1.0 } else { 0.0 });
        if on {
            // A slot that was skipped has stale state; start it from silence instead of from
            // whatever it happened to hold when it was switched off.
            match slot {
                FxSlot::HighPass => {
                    for filter in &mut self.hp {
                        filter.reset();
                    }
                }
                FxSlot::Eq => {
                    for band in self.bands.iter_mut().flatten() {
                        band.reset();
                    }
                }
                FxSlot::Comp => self.comp.reset(),
                FxSlot::Delay | FxSlot::Reverb => {}
            }
        }
        self.preset = FxPreset::Custom;
    }

    /// Load a named chain. Everything ramps, so this is safe to do while the music runs.
    pub fn load_preset(&mut self, preset: FxPreset) {
        let settings = preset.settings();
        self.apply_settings(settings);
        self.set_bypass_silently(preset.bypasses());
        self.preset = preset;
    }

    /// Bypass without marking the preset as edited, for `load_preset`.
    fn set_bypass_silently(&mut self, on: bool) {
        self.bypass = on;
        self.bypass_mix.set(if on { 0.0 } else { 1.0 });
        if !on {
            self.reset_states();
        }
    }

    fn apply_settings(&mut self, settings: ChainSettings) {
        for (i, &on) in settings.enabled.iter().enumerate() {
            self.slot_mix[i].set(if on { 1.0 } else { 0.0 });
        }
        self.set = settings;
        self.set.high_pass_hz = settings.high_pass_hz.clamp(20.0, 1_000.0);
        for band in self.set.bands.iter_mut() {
            *band = band.clamped();
        }
        self.rebuild_high_pass();
        self.rebuild_bands();
        self.comp.set_settings(settings.comp);
        self.set.comp = self.comp.settings();
        self.delay.set_note(settings.delay_note);
        self.delay.set_feedback(settings.delay_feedback);
        self.delay.set_mix(settings.delay_mix);
        self.set.delay_feedback = self.delay.feedback();
        self.set.delay_mix = self.delay.mix();
        self.reverb.set_size(settings.reverb_size);
        self.reverb.set_damping(settings.reverb_damping);
        self.reverb.set_mix(settings.reverb_mix);
        self.set.reverb_size = self.reverb.size();
        self.set.reverb_damping = self.reverb.damping();
        self.set.reverb_mix = self.reverb.mix();
        self.refresh_tail();
    }

    /// One knob. Marks the chain as edited, so the display stops claiming a preset it no longer is.
    pub fn set_param(&mut self, param: FxParam) {
        match param {
            FxParam::HighPassHz(hz) => {
                self.set.high_pass_hz = hz.clamp(20.0, 1_000.0);
                self.rebuild_high_pass();
            }
            FxParam::BandKind { band, kind } => {
                if let Some(b) = self.set.bands.get_mut(band) {
                    b.kind = kind;
                    self.rebuild_band(band);
                }
            }
            FxParam::BandHz { band, hz } => {
                if let Some(b) = self.set.bands.get_mut(band) {
                    b.hz = hz.clamp(20.0, 20_000.0);
                    self.rebuild_band(band);
                }
            }
            FxParam::BandQ { band, q } => {
                if let Some(b) = self.set.bands.get_mut(band) {
                    b.q = q.clamp(0.1, 12.0);
                    self.rebuild_band(band);
                }
            }
            FxParam::BandGainDb { band, db } => {
                if let Some(b) = self.set.bands.get_mut(band) {
                    let was_flat = b.is_flat();
                    b.gain_db = db.clamp(-24.0, 24.0);
                    let is_flat = b.gain_db == 0.0;
                    if was_flat && !is_flat && let Some(pair) = self.bands.get_mut(band) {
                        // The band was being skipped; it must not ring with old samples.
                        for f in pair.iter_mut() {
                            f.reset();
                        }
                    }
                    self.rebuild_band(band);
                }
            }
            FxParam::CompThresholdDb(v) => self.set_comp(|c| c.threshold_db = v),
            FxParam::CompRatio(v) => self.set_comp(|c| c.ratio = v),
            FxParam::CompAttackMs(v) => self.set_comp(|c| c.attack_ms = v),
            FxParam::CompReleaseMs(v) => self.set_comp(|c| c.release_ms = v),
            FxParam::CompKneeDb(v) => self.set_comp(|c| c.knee_db = v),
            FxParam::CompMakeupDb(v) => self.set_comp(|c| c.makeup_db = v),
            FxParam::DelayNote(note) => {
                self.set.delay_note = note;
                self.delay.set_note(note);
                self.refresh_tail();
            }
            FxParam::DelayFeedback(v) => {
                self.delay.set_feedback(v);
                self.set.delay_feedback = self.delay.feedback();
                self.refresh_tail();
            }
            FxParam::DelayMix(v) => {
                self.delay.set_mix(v);
                self.set.delay_mix = self.delay.mix();
            }
            FxParam::ReverbSize(v) => {
                self.reverb.set_size(v);
                self.set.reverb_size = self.reverb.size();
                self.refresh_tail();
            }
            FxParam::ReverbDamping(v) => {
                self.reverb.set_damping(v);
                self.set.reverb_damping = self.reverb.damping();
                self.refresh_tail();
            }
            FxParam::ReverbMix(v) => {
                self.reverb.set_mix(v);
                self.set.reverb_mix = self.reverb.mix();
            }
        }
        self.preset = FxPreset::Custom;
    }

    fn set_comp(&mut self, edit: impl FnOnce(&mut CompSettings)) {
        let mut c = self.set.comp;
        edit(&mut c);
        self.comp.set_settings(c);
        self.set.comp = self.comp.settings();
    }

    fn rebuild_high_pass(&mut self) {
        // Butterworth Q: maximally flat pass band, exactly -3 dB at the corner.
        let coeffs = Coeffs::high_pass(
            self.sample_rate,
            self.set.high_pass_hz,
            std::f32::consts::FRAC_1_SQRT_2,
        );
        for filter in &mut self.hp {
            filter.set(coeffs);
        }
    }

    fn rebuild_band(&mut self, index: usize) {
        let Some(&s) = self.set.bands.get(index) else {
            return;
        };
        let coeffs = Coeffs::band(self.sample_rate, s.kind, s.hz, s.q, s.gain_db);
        if let Some(pair) = self.bands.get_mut(index) {
            for f in pair.iter_mut() {
                f.set(coeffs);
            }
        }
    }

    fn rebuild_bands(&mut self) {
        for i in 0..EQ_BANDS {
            self.rebuild_band(i);
        }
    }

    fn reset_states(&mut self) {
        for filter in &mut self.hp {
            filter.reset();
        }
        for band in self.bands.iter_mut().flatten() {
            band.reset();
        }
        self.comp.reset();
    }

    /// Longest silence after which nothing can come out of the chain any more.
    fn refresh_tail(&mut self) {
        let delay_tail = {
            let feedback = self.delay.feedback().clamp(0.0, 0.95) as f64;
            let passes = if feedback <= 0.001 {
                1.0
            } else {
                -60.0 / (20.0 * feedback.log10())
            };
            (self.delay.target_tap_samples() as f64 * passes).min((self.sample_rate * 30) as f64)
                as u32
        };
        let margin = (TAIL_MARGIN_MS * 0.001 * self.sample_rate as f32) as u32;
        self.tail_samples = delay_tail
            .saturating_add(self.reverb.tail_samples())
            .saturating_add(margin);
    }

    // ---- the audio path ----------------------------------------------------------------------

    /// One stereo frame through the whole chain.
    ///
    /// A mono track hands in `Frame::mono(x)`, i.e. the same sample on both channels; from here on
    /// there is no difference between a mono and a stereo track, which is what keeps this function
    /// free of special cases.
    ///
    /// The three early exits are what makes eight of these affordable: a bypassed chain returns
    /// its input untouched, an idle chain returns silence, and a switched-off slot is skipped
    /// rather than multiplied by zero.
    #[inline]
    pub fn process(&mut self, x: Frame) -> Frame {
        // Bit-exact bypass. Only once the crossfade has really arrived at zero - before that the
        // chain is still fading out and has to keep running.
        if self.bypass && self.bypass_mix.settled() && self.bypass_mix.value() == 0.0 {
            self.silence_run = u32::MAX;
            return x;
        }

        // Idle: nothing has gone in for longer than the longest tail, so nothing can come out.
        // Both channels have to be silent - one side still ringing keeps the whole chain awake.
        if x.is_silent() {
            if self.silence_run >= self.tail_samples {
                return Frame::SILENT;
            }
            self.silence_run += 1;
        } else {
            self.silence_run = 0;
        }

        let mut y = x;

        // 1. High-pass, one filter instance per channel.
        let m = self.slot_mix[0].next();
        if m > 0.0 {
            let w = Frame {
                l: self.hp[0].process(y.l),
                r: self.hp[1].process(y.r),
            };
            y = blend(y, w, m);
        }

        // 2. Three EQ bands. A band at 0 dB is a wire, so it is not computed at all.
        let m = self.slot_mix[1].next();
        if m > 0.0 {
            let mut w = y;
            for i in 0..EQ_BANDS {
                if !self.set.bands[i].is_flat() {
                    w = Frame {
                        l: self.bands[i][0].process(w.l),
                        r: self.bands[i][1].process(w.r),
                    };
                }
            }
            y = blend(y, w, m);
        }

        // 3. Compressor - one detector, one gain, both channels. See `dynamics`.
        let m = self.slot_mix[2].next();
        if m > 0.0 {
            let w = self.comp.process(y);
            y = blend(y, w, m);
        }

        // 4. Delay - after the compressor, so the repeats are not themselves compressed and the
        //    tail keeps the dynamics of the note it came from.
        let m = self.slot_mix[3].next();
        if m > 0.0 {
            let w = self.delay.process(y);
            y = blend(y, w, m);
        }

        // 5. Reverb, last, so it hears the delay's repeats as well - which is what puts them in
        //    the same room instead of in front of it.
        let m = self.slot_mix[4].next();
        if m > 0.0 {
            let w = self.reverb.process(y);
            y = blend(y, w, m);
        }

        let b = self.bypass_mix.next();
        blend(x, y, b)
    }
}

/// Crossfade that is exact at both ends: at 1.0 it returns the wet frame itself rather than
/// `dry + (wet - dry)`, which in floating point is not always the same number.
#[inline(always)]
fn blend(dry: Frame, wet: Frame, mix: f32) -> Frame {
    if mix >= 1.0 {
        wet
    } else {
        Frame {
            l: dry.l + (wet.l - dry.l) * mix,
            r: dry.r + (wet.r - dry.r) * mix,
        }
    }
}

#[cfg(test)]
mod tests;
