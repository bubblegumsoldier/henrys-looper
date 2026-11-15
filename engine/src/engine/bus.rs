//! The two output buses, what may be sent to them, and where they leave the machine.
//!
//! # Why two buses and not one
//!
//! The musician needs the click; the room must not have it. With a single output that is not a
//! setting, it is a contradiction: everything the engine produces goes to the same socket, so the
//! metronome is either missing from the headphones or present in the PA. Splitting the sum into
//! two buses is what makes the sentence *"Klick nur auf den Monitorweg"* (`docs/architektur.md`,
//! section 0) expressible at all.
//!
//! ```text
//!                        ┌────────────────► Main    ─► Ausgangspaar A  (Saal)
//!  Loop je Track   ──────┤
//!  Mithoeren je Track ───┤
//!  Klick ────────────────┴─(nur hierhin)──► Monitor ─► Ausgangspaar B  (Kopfhoerer)
//! ```
//!
//! Both buses are stereo, both have their own volume, and every source decides for itself which of
//! them it feeds - ordinary mixing-desk thinking. The one rule that is not a setting: **the click
//! never reaches Main.** It is not a mix decision, it is the reason the bus split exists, and a
//! switch that can put it back into the room would eventually be pressed by accident.
//!
//! # Two outputs, four outputs
//!
//! Separate buses need at least four device output channels. The machine this was written for - a
//! Focusrite Scarlett 2i2, whose headphone socket is a copy of outputs 1/2 - has two. That case is
//! not an error and is not silently repaired: both buses land on the same pair, are summed there,
//! and the program says so in one clear German sentence ([`routing_note`]). Silently switching the
//! click off would be worse than the click in the room, because the musician would then lose the
//! beat on stage and never find out why.
//!
//! The way out that experienced musicians use with two outputs is in here as well, and it needs no
//! mode of its own: a bus may be routed to a **single** channel, so `--bus-out main:1
//! --bus-out monitor:2` puts the mix on the left and the click on the right, to be split with a
//! Y-cable. That falls out of letting a bus name any channel; a separate "split mode" would be a
//! third thing to explain for something the general mechanism already does.

use std::fmt;

/// How many output buses there are. Two: the room and the musician.
pub const BUS_COUNT: usize = 2;

/// Channels in one bus. Every bus is stereo, whatever the tracks are.
pub const BUS_CHANNELS: usize = 2;

/// Samples in one engine output frame: both buses, interleaved as `[main_l, main_r, mon_l, mon_r]`.
pub const BUS_SAMPLES: usize = BUS_COUNT * BUS_CHANNELS;

/// Highest bus volume that can be dialled in, as a linear factor. +12 dB, the same headroom a
/// layer gain has.
pub const MAX_BUS_GAIN: f32 = 4.0;

/// One of the two buses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bus {
    /// To the PA. Never carries the click.
    Main,
    /// To the headphones. Carries the click.
    Monitor,
}

impl Bus {
    pub const ALL: [Bus; BUS_COUNT] = [Bus::Main, Bus::Monitor];

    #[inline(always)]
    pub fn index(self) -> usize {
        match self {
            Bus::Main => 0,
            Bus::Monitor => 1,
        }
    }

    /// From an index; anything out of range is Main, so a bad number cannot panic in a callback.
    #[inline(always)]
    pub fn from_index(index: usize) -> Self {
        if index == 1 { Bus::Monitor } else { Bus::Main }
    }

    /// The word the wire format and the command line use.
    pub fn name(self) -> &'static str {
        match self {
            Bus::Main => "main",
            Bus::Monitor => "monitor",
        }
    }

    /// German label for the display.
    pub fn label(self) -> &'static str {
        match self {
            Bus::Main => "Main (room)",
            Bus::Monitor => "Monitor (headphones)",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "main" | "pa" | "saal" => Some(Bus::Main),
            "monitor" | "mon" | "kopfhoerer" | "cue" => Some(Bus::Monitor),
            _ => None,
        }
    }
}

impl fmt::Display for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which buses one source feeds. A bit per bus, so a per-track pair of these costs two bytes in the
/// status snapshot rather than four bools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BusSend(u8);

impl BusSend {
    /// Heard nowhere. Not a broken state: a guide track that is only wanted later sits here.
    pub const NONE: BusSend = BusSend(0);
    pub const MAIN: BusSend = BusSend(1);
    pub const MONITOR: BusSend = BusSend(2);
    pub const BOTH: BusSend = BusSend(3);

    /// From the raw bits, ignoring anything above the two that exist.
    #[inline(always)]
    pub fn from_bits(bits: u8) -> Self {
        BusSend(bits & 0b11)
    }

    #[inline(always)]
    pub fn bits(self) -> u8 {
        self.0
    }

    #[inline(always)]
    pub fn on(self, bus: Bus) -> bool {
        self.0 & (1 << bus.index()) != 0
    }

    #[must_use]
    #[inline(always)]
    pub fn with(self, bus: Bus, on: bool) -> Self {
        let bit = 1 << bus.index();
        BusSend(if on { self.0 | bit } else { self.0 & !bit })
    }

    #[inline(always)]
    pub fn is_silent(self) -> bool {
        self.0 == 0
    }

    /// German label: what a track line and a tooltip say.
    pub fn label(self) -> &'static str {
        match self.0 {
            0 => "nowhere",
            1 => "Main",
            2 => "Monitor",
            _ => "Main+Monitor",
        }
    }

    /// Short label for a crowded terminal column.
    pub fn short(self) -> &'static str {
        match self.0 {
            0 => "--",
            1 => "M-",
            2 => "-H",
            _ => "MH",
        }
    }

    /// `main`, `monitor`, `main+monitor`, `none` - what the command line and the score write.
    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "none",
            1 => "main",
            2 => "monitor",
            _ => "main+monitor",
        }
    }

    /// Parse `main`, `monitor`, `main+monitor` (also `both`), `none`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        match text.as_str() {
            "none" | "aus" | "-" => return Some(BusSend::NONE),
            "both" | "beide" | "alle" => return Some(BusSend::BOTH),
            _ => {}
        }
        let mut send = BusSend::NONE;
        for part in text.split(['+', ',', '/']) {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            send = send.with(Bus::parse(part)?, true);
        }
        Some(send)
    }
}

/// Which of a track's two sources a routing decision is about.
///
/// They are separate because they answer different questions. The loop asks *where this recording
/// belongs*; monitoring asks *where the musician needs to hear himself*. On a stage with a PA that
/// already has the microphone, the second answer is "headphones only" while the first is "both".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackSource {
    /// The recorded layers of the track.
    Loop,
    /// The live input, while monitoring is switched on.
    Monitor,
}

impl TrackSource {
    pub const ALL: [TrackSource; 2] = [TrackSource::Loop, TrackSource::Monitor];

    pub fn name(self) -> &'static str {
        match self {
            TrackSource::Loop => "loop",
            TrackSource::Monitor => "monitor",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TrackSource::Loop => "Loop",
            TrackSource::Monitor => "Monitor",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "loop" | "wiedergabe" => Some(TrackSource::Loop),
            "monitor" | "mithoeren" | "mithören" => Some(TrackSource::Monitor),
            _ => None,
        }
    }
}

/// Where one bus leaves the machine: a stereo pair, or - on a device that has no second pair to
/// spare - a single channel the bus is folded down to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusOutput {
    /// First device output channel, zero-based.
    pub first: usize,
    /// 1 or 2. One means the bus is summed to mono at half level onto `first`; that is what makes
    /// "Main links, Klick rechts" possible on a two-output interface.
    pub width: usize,
}

impl BusOutput {
    pub const fn pair(first: usize) -> Self {
        Self { first, width: 2 }
    }

    pub const fn mono(channel: usize) -> Self {
        Self {
            first: channel,
            width: 1,
        }
    }

    /// Channels this bus occupies, as a zero-based half-open range.
    #[inline(always)]
    pub fn range(self) -> std::ops::Range<usize> {
        self.first..self.first + self.width
    }

    /// The same output reduced to what a device with `channels` outputs can actually carry.
    ///
    /// A pair whose right half does not exist becomes a mono fold onto its left half rather than
    /// half a stereo signal - dropping the right channel would silence everything panned hard
    /// right, which is a far nastier surprise than a fold-down. A bus whose channel does not exist
    /// at all moves to the last one there is, because a bus that is inaudible is not a routing, it
    /// is a mistake.
    pub fn clamped(self, channels: usize) -> Self {
        if channels == 0 {
            return BusOutput::mono(0);
        }
        let mut first = self.first.min(channels - 1);
        let mut width = self.width.clamp(1, 2);
        if first + width > channels {
            // Prefer keeping the pair by moving it down, which is what a user who asked for a pair
            // wants; only fold when there is genuinely just one channel left.
            if width == 2 && channels >= 2 {
                first = channels - 2;
            } else {
                width = 1;
            }
        }
        Self { first, width }
    }

    /// `1-2` or `3`, one-based, as the channels are printed on the interface.
    pub fn label(self) -> String {
        if self.width >= 2 {
            format!("{}-{}", self.first + 1, self.first + 2)
        } else {
            format!("{}", self.first + 1)
        }
    }
}

/// Where both buses leave the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusRouting {
    outs: [BusOutput; BUS_COUNT],
}

impl Default for BusRouting {
    fn default() -> Self {
        Self {
            outs: [BusOutput::pair(0), BusOutput::pair(0)],
        }
    }
}

impl BusRouting {
    pub fn new(main: BusOutput, monitor: BusOutput) -> Self {
        Self {
            outs: [main, monitor],
        }
    }

    /// What a device with `channels` outputs gets without anybody choosing anything.
    ///
    /// Four or more channels: Main on 1/2, Monitor on 3/4 - the split the whole feature is for, and
    /// the arrangement every interface with two headphone-capable pairs is wired as. Fewer: both on
    /// 1/2, summed, because that is the only place there is. The second case is announced rather
    /// than repaired, see [`routing_note`].
    pub fn default_for(channels: usize) -> Self {
        let routing = if channels >= 4 {
            Self::new(BusOutput::pair(0), BusOutput::pair(2))
        } else {
            Self::new(BusOutput::pair(0), BusOutput::pair(0))
        };
        routing.clamped(channels)
    }

    #[inline(always)]
    pub fn get(&self, bus: Bus) -> BusOutput {
        self.outs[bus.index()]
    }

    pub fn set(&mut self, bus: Bus, out: BusOutput) {
        self.outs[bus.index()] = out;
    }

    #[must_use]
    pub fn with(mut self, bus: Bus, out: BusOutput) -> Self {
        self.set(bus, out);
        self
    }

    /// Every output reduced to what the device can carry.
    #[must_use]
    pub fn clamped(self, channels: usize) -> Self {
        Self {
            outs: [
                self.outs[0].clamped(channels),
                self.outs[1].clamped(channels),
            ],
        }
    }

    /// Whether the two buses share at least one device channel, i.e. whether the click ends up in
    /// the room.
    pub fn overlaps(&self) -> bool {
        let (a, b) = (self.outs[0].range(), self.outs[1].range());
        a.start < b.end && b.start < a.end
    }

    /// Whether the two buses are folded into **one** mix - true as soon as they share a single
    /// device channel, which is what a two-output interface forces.
    ///
    /// This is worth a name of its own, because the engine mixes differently then: a channel that
    /// carries both buses is one way out, and one way out is one mix, so every source is summed
    /// **once** whichever buses it is assigned to. Summing the two buses on top of each other would
    /// put a track that goes to both 6 dB over a track that goes to one, and would make plugging in
    /// a second output pair change the balance of the first. The rule is per channel and therefore
    /// holds for a partial overlap too: [`place_buses`] puts the one mix on every channel either bus
    /// uses, exactly once. See `process::render`.
    pub fn collapsed(&self) -> bool {
        self.overlaps()
    }

    /// Whether both buses leave on exactly the same channels - the normal case on a two-output
    /// interface, as opposed to a partial overlap somebody chose.
    pub fn identical(&self) -> bool {
        self.outs[0] == self.outs[1]
    }

    /// Highest device channel either bus uses, one-based - what a device has to be able to offer.
    pub fn highest_channel(&self) -> usize {
        self.outs.iter().map(|o| o.first + o.width).max().unwrap_or(0)
    }
}

/// The sentence to print about a routing, or `None` when the two buses are properly apart.
///
/// Deliberately one sentence and deliberately not a warning that repeats itself: it is printed once
/// at startup and shown once in the setup panel. A message that nags on every status update trains
/// the musician to stop reading messages, which is expensive on the one evening a message matters.
pub fn routing_note(routing: &BusRouting, channels: usize) -> Option<String> {
    if !routing.overlaps() {
        return None;
    }
    let main = routing.get(Bus::Main);
    let monitor = routing.get(Bus::Monitor);
    if !routing.identical() {
        return Some(format!(
            "Main is on output {}, Monitor on output {} - they partly overlap, so there is one \
             mix at the Main level on every one of those channels, each source in it once, and \
             the click goes out to the room as well. \
             To separate them, put Main on outputs 1-2 and Monitor on 3-4.",
            main.label(),
            monitor.label()
        ));
    }
    let head = if channels < 4 {
        format!(
            "The device has {channels} output channels. Separate buses need four. \
             Main and Monitor are therefore both on output {}",
            main.label()
        )
    } else {
        format!(
            "Main and Monitor are both on output {} - with {channels} channels the device \
             would have room for separate pairs",
            main.label()
        )
    };
    Some(format!(
        "{head}: there is one way out and therefore one mix. Every source is in it exactly \
         once, so levels stay as they would be - but the click goes out to the room too, \
         the Main level sets the whole output and the Monitor level has nothing of its own \
         to control. \
         With four outputs, put Main on 1-2 and Monitor on 3-4. \
         Stopgap with two: Main on output 1 and Monitor on output 2 puts the mix on the left \
         and the click on the right, to be split with a Y cable."
    ))
}

/// Copy one engine output frame - both buses - into one device output frame.
///
/// This replaces the old `spread_frame`, which put the single bus on every device channel pair. It
/// is the one place a bus becomes a socket, and it has exactly three properties worth stating:
///
/// * **Every source reaches a socket once.** Buses that share a channel are one mix
///   ([`BusRouting::collapsed`]): the engine has already summed every source once into Main and
///   left Monitor silent, and here that one mix (Main plus whatever Monitor holds) is placed on
///   every channel either bus uses - with Main's layout where Main is, with Monitor's where only
///   Monitor is - and never twice on the same channel. Nothing is dropped and nothing is doubled.
/// * **A bus of width 1 is folded to mono at half level**, the same fold the old code did for a
///   single-channel device: dropping one side would silence anything panned hard to it.
/// * **Channels the device does not have are skipped**, not written past the end.
///
/// `buses` is `[main_l, main_r, mon_l, mon_r]`; `out` is one interleaved device frame.
#[inline]
pub fn place_buses(buses: &[f32], out: &mut [f32], routing: &BusRouting) {
    for slot in out.iter_mut() {
        *slot = 0.0;
    }
    let side = |bus: Bus, fallback: f32| -> (f32, f32) {
        let base = bus.index() * BUS_CHANNELS;
        let l = buses.get(base).copied().unwrap_or(fallback);
        let r = buses.get(base + 1).copied().unwrap_or(l);
        (l, r)
    };
    let write = |out: &mut [f32], target: BusOutput, (l, r): (f32, f32), skip: Option<BusOutput>| {
        let free = |c: usize| skip.is_none_or(|s| !s.range().contains(&c));
        if target.width >= 2 {
            for (c, v) in [(target.first, l), (target.first + 1, r)] {
                if free(c) {
                    if let Some(slot) = out.get_mut(c) {
                        *slot += v;
                    }
                }
            }
        } else if free(target.first) {
            if let Some(slot) = out.get_mut(target.first) {
                *slot += (l + r) * 0.5;
            }
        }
    };
    let main = routing.get(Bus::Main);
    let monitor = routing.get(Bus::Monitor);
    if routing.collapsed() {
        let (ml, mr) = side(Bus::Main, 0.0);
        let (nl, nr) = side(Bus::Monitor, 0.0);
        let one = (ml + nl, mr + nr);
        write(out, main, one, None);
        write(out, monitor, one, Some(main));
    } else {
        write(out, main, side(Bus::Main, 0.0), None);
        write(out, monitor, side(Bus::Monitor, 0.0), None);
    }
}

/// Device output channels that get a meter of their own in the status snapshot. Every channel is
/// limited; only the first eight are metered, which covers every interface this is played on
/// without making the snapshot grow with the device.
pub const OUT_METERS: usize = 8;

/// Where the safety limiter on every device channel starts to act: -0.3 dBFS.
pub const LIMIT_THRESHOLD: f32 = 0.966_050_9;

/// The soft clipper at the very end of [`OutputLimiter`], on its own: no state, no latency.
///
/// Below [`LIMIT_THRESHOLD`] the sample passes **bit for bit**; above it a `tanh` knee bends it
/// towards full scale, which it approaches but never exceeds. The knee starts with slope 1, so
/// there is no edge at the threshold - an over becomes a rounded peak instead of the hard digital
/// crackle a plain clamp makes. A sample that is not a number at all becomes silence. On its own
/// it is only a gentler clip; the limiter in front of it is what keeps it from having to work.
#[inline(always)]
pub fn safety_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= LIMIT_THRESHOLD {
        return x;
    }
    if !x.is_finite() {
        return if x.is_nan() { 0.0 } else { x.signum() };
    }
    let room = 1.0 - LIMIT_THRESHOLD;
    let y = LIMIT_THRESHOLD + room * ((a - LIMIT_THRESHOLD) / room).tanh();
    y.min(1.0).copysign(x)
}

/// How fast the limiter pulls the gain down once a sample is over: 0.5 ms time constant.
const LIMIT_ATTACK_S: f32 = 0.000_5;
/// How long it holds the reduction after the last over, so a low note between two of its peaks
/// is not modulated: 10 ms.
const LIMIT_HOLD_S: f32 = 0.010;
/// How fast the gain comes back after the hold: 80 ms time constant.
const LIMIT_RELEASE_S: f32 = 0.080;

/// One device channel's limiter state.
#[derive(Clone, Copy, Debug)]
struct LimiterChannel {
    /// The gain the overs ask for, held and then released towards 1.
    target: f32,
    /// The gain applied, following `target` down at the attack rate and up with it.
    gain: f32,
    /// Samples left before `target` may start to release.
    hold: u32,
}

impl LimiterChannel {
    const IDLE: Self = Self {
        target: 1.0,
        gain: 1.0,
        hold: 0,
    };
}

/// The safety limiter on every device output channel, in the output stage
/// (`EngineCore::place_output`), after the buses have been placed on the sockets.
///
/// A peak limiter **without lookahead**, because lookahead is latency: it would delay every socket,
/// and the looper's whole timing rests on a roundtrip that is measured once and then never moves.
/// Per channel and per sample:
///
/// * A sample above [`LIMIT_THRESHOLD`] (-0.3 dBFS) asks for the gain that brings it down to the
///   threshold. The lowest such gain is held for 10 ms and then released towards 1 with an 80 ms
///   time constant; the applied gain follows it down within about half a millisecond. So an over
///   turns the whole signal down for a moment - the waveform keeps its shape - instead of cutting
///   its tops off.
/// * Whatever is still over while the gain is on its way down goes through [`safety_limit`], the
///   soft clipper: rounded, never above full scale.
/// * Nothing over, and the gain fully back at 1: the sample passes **bit for bit**. The limiter is
///   then not there.
///
/// The state is allocated when the engine is built; `process` neither allocates nor locks.
#[derive(Clone, Debug)]
pub struct OutputLimiter {
    channels: Vec<LimiterChannel>,
    attack: f32,
    release: f32,
    hold: u32,
}

impl OutputLimiter {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        // In f64 and rounded once: the same coefficient whether the compiler folds it from a
        // constant rate or the engine computes it from the device's, so two limiters built for the
        // same rate agree to the bit.
        let rate = f64::from(sample_rate.max(1));
        let coeff = |seconds: f32| (1.0 - (-1.0 / (f64::from(seconds) * rate)).exp()) as f32;
        Self {
            channels: vec![LimiterChannel::IDLE; channels],
            attack: coeff(LIMIT_ATTACK_S),
            release: coeff(LIMIT_RELEASE_S),
            hold: (f64::from(LIMIT_HOLD_S) * rate).round() as u32,
        }
    }

    /// One sample of device channel `channel`. A channel the limiter was not built for gets the
    /// stateless soft clipper, which still never lets anything past full scale.
    #[inline(always)]
    pub fn process(&mut self, channel: usize, x: f32) -> f32 {
        let Some(ch) = self.channels.get_mut(channel) else {
            return safety_limit(x);
        };
        let a = x.abs();
        if a > LIMIT_THRESHOLD && a.is_finite() {
            let need = LIMIT_THRESHOLD / a;
            if need <= ch.target {
                ch.target = need;
                ch.hold = self.hold;
            }
        }
        if ch.target < 1.0 {
            if ch.hold > 0 {
                ch.hold -= 1;
            } else {
                ch.target += (1.0 - ch.target) * self.release;
                // The last step home is taken at once: -0.009 dB is inaudible, and an exponential
                // in f32 stalls a hair below 1 (the step falls under half an ulp) instead of
                // arriving - the limiter would never be bit-transparent again.
                if ch.target > 0.999 {
                    ch.target = 1.0;
                }
            }
        }
        if ch.gain > ch.target {
            ch.gain += (ch.target - ch.gain) * self.attack;
            if ch.gain - ch.target < 1e-6 {
                ch.gain = ch.target;
            }
        } else {
            ch.gain = ch.target;
        }
        if ch.gain == 1.0 {
            safety_limit(x)
        } else {
            safety_limit(x * ch.gain)
        }
    }

    /// Whether every channel is idle - nothing over, gain fully back.
    pub fn idle(&self) -> bool {
        self.channels.iter().all(|c| c.gain == 1.0 && c.target == 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_is_two_independent_bits() {
        let mut send = BusSend::NONE;
        assert!(send.is_silent());
        assert!(!send.on(Bus::Main));
        send = send.with(Bus::Monitor, true);
        assert_eq!(send, BusSend::MONITOR);
        assert!(send.on(Bus::Monitor));
        assert!(!send.on(Bus::Main));
        send = send.with(Bus::Main, true);
        assert_eq!(send, BusSend::BOTH);
        send = send.with(Bus::Monitor, false);
        assert_eq!(send, BusSend::MAIN);
        assert_eq!(BusSend::from_bits(0xff), BusSend::BOTH);
        assert_eq!(BusSend::BOTH.bits(), 0b11);
    }

    #[test]
    fn a_send_reads_and_writes_the_same_words() {
        for send in [BusSend::NONE, BusSend::MAIN, BusSend::MONITOR, BusSend::BOTH] {
            assert_eq!(BusSend::parse(send.name()), Some(send), "{}", send.name());
        }
        assert_eq!(BusSend::parse("both"), Some(BusSend::BOTH));
        assert_eq!(BusSend::parse(" Main + Monitor "), Some(BusSend::BOTH));
        assert_eq!(BusSend::parse("monitor,main"), Some(BusSend::BOTH));
        assert_eq!(BusSend::parse(""), None);
        assert_eq!(BusSend::parse("saal+quatsch"), None);
        assert_eq!(BusSend::BOTH.label(), "Main+Monitor");
        assert_eq!(BusSend::NONE.short(), "--");
    }

    /// Four outputs get the split the feature exists for; two get the honest fallback.
    #[test]
    fn the_default_routing_splits_as_soon_as_there_are_four_outputs() {
        let four = BusRouting::default_for(4);
        assert_eq!(four.get(Bus::Main), BusOutput::pair(0));
        assert_eq!(four.get(Bus::Monitor), BusOutput::pair(2));
        assert!(!four.overlaps());
        assert!(routing_note(&four, 4).is_none());

        let two = BusRouting::default_for(2);
        assert_eq!(two.get(Bus::Main), BusOutput::pair(0));
        assert_eq!(two.get(Bus::Monitor), BusOutput::pair(0));
        assert!(two.overlaps());
        assert!(two.collapsed(), "ein Weg hinaus ist eine Mischung");
        assert!(two.identical());
        let note = routing_note(&two, 2).expect("zwei Ausgaenge werden erklaert");
        assert!(note.contains("four"), "{note}");
        assert!(note.contains("click"), "{note}");
        assert!(note.contains("exactly once"), "keine Verdopplung: {note}");
        assert!(note.contains("Y cable"), "der Notbehelf steht drin: {note}");

        // A partial overlap is a routing somebody chose; it is one mix too, and says so.
        let odd = BusRouting::new(BusOutput::pair(0), BusOutput::pair(1));
        assert!(odd.overlaps());
        assert!(odd.collapsed(), "ein geteilter Kanal reicht fuer eine Mischung");
        assert!(!odd.identical());
        let note = routing_note(&odd, 4).expect("Ueberschneidung wird erklaert");
        assert!(note.contains("partly"), "{note}");

        // Eight outputs still default to the first two pairs; more is a routing decision.
        let eight = BusRouting::default_for(8);
        assert_eq!(eight.get(Bus::Monitor), BusOutput::pair(2));
    }

    #[test]
    fn an_output_is_clamped_to_what_the_device_has() {
        assert_eq!(BusOutput::pair(2).clamped(4), BusOutput::pair(2));
        // Asked for 3-4 on a two-output device: the pair moves down rather than losing a side.
        assert_eq!(BusOutput::pair(2).clamped(2), BusOutput::pair(0));
        // One single output channel: everything folds onto it.
        assert_eq!(BusOutput::pair(0).clamped(1), BusOutput::mono(0));
        assert_eq!(BusOutput::mono(5).clamped(2), BusOutput::mono(1));
        assert_eq!(BusOutput::pair(0).clamped(0), BusOutput::mono(0));
        assert_eq!(BusOutput::pair(0).label(), "1-2");
        assert_eq!(BusOutput::mono(2).label(), "3");
    }

    /// Four channels: the two buses lie apart and nothing bleeds onto the other pair.
    #[test]
    fn four_outputs_keep_the_two_buses_apart() {
        let routing = BusRouting::default_for(4);
        let mut out = [9.0f32; 4];
        place_buses(&[0.5, -0.25, 0.125, 1.0], &mut out, &routing);
        assert_eq!(out, [0.5, -0.25, 0.125, 1.0]);

        // Only the click on the monitor bus: the main pair stays digitally silent.
        let mut out = [9.0f32; 4];
        place_buses(&[0.0, 0.0, 0.7, 0.7], &mut out, &routing);
        assert_eq!(out, [0.0, 0.0, 0.7, 0.7]);
    }

    /// Two channels: both buses land on the same pair and are summed, not one of them dropped.
    #[test]
    fn two_outputs_sum_both_buses_onto_the_one_pair() {
        let routing = BusRouting::default_for(2);
        let mut out = [9.0f32; 2];
        place_buses(&[0.5, -0.25, 0.125, 0.5], &mut out, &routing);
        assert_eq!(out, [0.625, 0.25], "nichts geht verloren, es wird summiert");
    }

    /// The stopgap: mix on the left, click on the right, both folded to mono.
    #[test]
    fn a_bus_of_width_one_is_folded_to_mono_at_half_level() {
        let routing = BusRouting::new(BusOutput::mono(0), BusOutput::mono(1));
        assert!(!routing.overlaps());
        let mut out = [9.0f32; 2];
        place_buses(&[1.0, 0.5, 0.4, 0.2], &mut out, &routing);
        assert_eq!(out, [0.75, 0.3]);

        // A device with one single channel: both buses fold onto it and are summed.
        let routing = BusRouting::default_for(1);
        let mut out = [9.0f32; 1];
        place_buses(&[1.0, 1.0, 0.5, 0.5], &mut out, &routing);
        assert_eq!(out, [1.5]);
    }

    /// A routing that names channels the device does not have must not write past the end.
    #[test]
    fn channels_the_device_lacks_are_skipped_rather_than_written_past() {
        let routing = BusRouting::new(BusOutput::pair(0), BusOutput::pair(6));
        let mut out = [9.0f32; 2];
        place_buses(&[0.5, 0.5, 1.0, 1.0], &mut out, &routing);
        assert_eq!(out, [0.5, 0.5]);
        assert_eq!(routing.highest_channel(), 8);
    }

    /// A partial overlap, channel by channel: the shared channel carries the one mix once, the
    /// channels only one bus uses carry it as well, nothing is added twice.
    #[test]
    fn a_partial_overlap_puts_the_one_mix_once_on_every_channel() {
        // Main on 1-2, Monitor on 2-3: channel 2 is shared.
        let routing = BusRouting::new(BusOutput::pair(0), BusOutput::pair(1));
        let mut out = [9.0f32; 4];
        place_buses(&[0.5, -0.25, 0.0, 0.0], &mut out, &routing);
        assert_eq!(out, [0.5, -0.25, -0.25, 0.0], "Kanal 2 einmal, Kanal 3 die rechte Seite");

        // Main folded onto 1, Monitor on 1-2: channel 1 is shared and gets Main's fold only.
        let routing = BusRouting::new(BusOutput::mono(0), BusOutput::pair(0));
        let mut out = [9.0f32; 2];
        place_buses(&[0.5, 0.25, 0.0, 0.0], &mut out, &routing);
        assert_eq!(out, [0.375, 0.25]);

        // No overlap at all: each bus on its own channels, exactly as before.
        let routing = BusRouting::new(BusOutput::pair(2), BusOutput::pair(0));
        assert!(!routing.collapsed());
        let mut out = [9.0f32; 4];
        place_buses(&[0.5, 0.25, 0.125, 0.0625], &mut out, &routing);
        assert_eq!(out, [0.125, 0.0625, 0.5, 0.25]);
    }

    /// Below the threshold the limiter is not there at all; above it the curve bends smoothly
    /// towards full scale and never past it.
    #[test]
    fn the_safety_limiter_is_transparent_below_and_soft_above_its_threshold() {
        assert!((20.0 * LIMIT_THRESHOLD.log10() + 0.3).abs() < 1e-4, "-0.3 dBFS");
        let mut x = -LIMIT_THRESHOLD;
        while x <= LIMIT_THRESHOLD {
            assert_eq!(safety_limit(x).to_bits(), x.to_bits(), "bitgleich bei {x}");
            x += 0.000_37;
        }
        assert_eq!(safety_limit(LIMIT_THRESHOLD), LIMIT_THRESHOLD);
        // Monotonic, odd, inside full scale, and without a kink: the slope just above the
        // threshold is still ~1, it only flattens further up.
        let mut last = LIMIT_THRESHOLD;
        let mut x = LIMIT_THRESHOLD;
        for _ in 0..4000 {
            x += 0.001;
            let y = safety_limit(x);
            assert!(y >= last && y <= 1.0 && y <= x, "bei {x}: {y}");
            assert_eq!(safety_limit(-x), -y);
            last = y;
        }
        let h = 1e-3;
        let slope = (safety_limit(LIMIT_THRESHOLD + h) - LIMIT_THRESHOLD) / h;
        assert!(slope > 0.95, "keine Kante an der Schwelle: Steigung {slope}");
        assert!(safety_limit(1.35) > 0.99 && safety_limit(1.35) <= 1.0);
        assert_eq!(safety_limit(f32::NAN), 0.0);
        assert_eq!(safety_limit(f32::INFINITY), 1.0);
        assert_eq!(safety_limit(f32::NEG_INFINITY), -1.0);
    }

    /// The limiter turns a sustained over down to the threshold within a millisecond, keeps the
    /// waveform's shape while it does, and afterwards comes back to exactly 1 - bit for bit again.
    #[test]
    fn the_output_limiter_rides_the_gain_and_lets_go_completely() {
        let rate = 48_000;
        let mut lim = OutputLimiter::new(rate, 1);
        let tone = |k: usize, amp: f32| amp * (std::f32::consts::TAU * 100.0 * k as f32 / 48_000.0).sin();
        for k in 0..4_800 {
            let x = tone(k, 0.9);
            assert_eq!(lim.process(0, x).to_bits(), x.to_bits(), "unter der Schwelle bitgleich");
        }
        assert!(lim.idle());
        // +3 dBFS for half a second: from the second period on nothing is above the threshold; the
        // first peak, while the gain is still on its way down, is the soft clipper's.
        let mut top = 0.0f32;
        for k in 0..24_000 {
            let y = lim.process(0, tone(k, 1.41));
            assert!(y.abs() <= 1.0);
            if k > 480 {
                top = top.max(y.abs());
            }
        }
        assert!(top <= LIMIT_THRESHOLD + 1e-5 && top > LIMIT_THRESHOLD - 0.01, "{top}");
        assert!(!lim.idle());
        // Quiet again: after the release the gain is exactly 1 and the samples pass untouched.
        for k in 0..96_000 {
            lim.process(0, tone(k, 0.5));
        }
        assert!(lim.idle(), "zwei Sekunden spaeter ist der Begrenzer wieder weg");
        let x = tone(17, 0.5);
        assert_eq!(lim.process(0, x).to_bits(), x.to_bits());
        // A channel the limiter was not built for still never passes full scale.
        assert_eq!(lim.process(5, 3.0), 1.0);
    }
}
