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
            Bus::Main => "Main (Saal)",
            Bus::Monitor => "Monitor (Kopfhoerer)",
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
            0 => "nirgends",
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
            2 => "-K",
            _ => "MK",
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
            TrackSource::Monitor => "Mithoeren",
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

    /// Whether both buses leave on **exactly** the same channels - the normal case on a
    /// two-output interface.
    ///
    /// This is worth a name of its own, because the engine mixes differently then: one way out is
    /// one mix, so every source is summed **once** whichever buses it is assigned to. Summing the
    /// two buses on top of each other would put a track that goes to both 6 dB over a track that
    /// goes to one, and would make plugging in a second output pair change the balance of the
    /// first. See `process::render`.
    pub fn collapsed(&self) -> bool {
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
    if !routing.collapsed() {
        return Some(format!(
            "Main liegt auf Ausgang {}, Monitor auf Ausgang {} - die beiden ueberschneiden sich \
             teilweise, auf dem gemeinsamen Kanal wird summiert und der Klick geht mit in den Saal. \
             Sauber getrennt z.B. mit --bus-out main:1-2 --bus-out monitor:3-4.",
            main.label(),
            monitor.label()
        ));
    }
    let head = if channels < 4 {
        format!(
            "Das Geraet hat {channels} Ausgangskanaele. Getrennte Busse brauchen vier. \
             Main und Monitor liegen deshalb beide auf Ausgang {}",
            main.label()
        )
    } else {
        format!(
            "Main und Monitor liegen beide auf Ausgang {} - das Geraet haette mit {channels} \
             Kanaelen Platz fuer getrennte Paare",
            main.label()
        )
    };
    Some(format!(
        "{head}: es gibt einen Weg hinaus und damit eine Mischung. Jede Quelle klingt darin genau \
         einmal, die Lautstaerken bleiben also, wie sie waeren - aber der Klick geht mit in den \
         Saal, und die Monitor-Lautstaerke hat nichts, was sie getrennt regeln koennte. \
         Mit vier Ausgaengen: --bus-out main:1-2 --bus-out monitor:3-4. \
         Notbehelf mit zweien: --bus-out main:1 --bus-out monitor:2 legt den Mix nach links und \
         den Klick nach rechts, mit einem Y-Kabel aufzutrennen."
    ))
}

/// Copy one engine output frame - both buses - into one device output frame.
///
/// This replaces the old `spread_frame`, which put the single bus on every device channel pair. It
/// is the one place a bus becomes a socket, and it has exactly three properties worth stating:
///
/// * **Overlap sums, it does not overwrite.** Two buses on the same pair is the normal case on a
///   two-output interface, and there the musician must hear *both*, not whichever was written last.
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
    for bus in Bus::ALL {
        let base = bus.index() * BUS_CHANNELS;
        let l = buses.get(base).copied().unwrap_or(0.0);
        let r = buses.get(base + 1).copied().unwrap_or(l);
        let target = routing.get(bus);
        if target.width >= 2 {
            if let Some(slot) = out.get_mut(target.first) {
                *slot += l;
            }
            if let Some(slot) = out.get_mut(target.first + 1) {
                *slot += r;
            }
        } else if let Some(slot) = out.get_mut(target.first) {
            *slot += (l + r) * 0.5;
        }
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
        let note = routing_note(&two, 2).expect("zwei Ausgaenge werden erklaert");
        assert!(note.contains("vier"), "{note}");
        assert!(note.contains("Klick"), "{note}");
        assert!(note.contains("genau einmal"), "keine Verdopplung: {note}");
        assert!(note.contains("Y-Kabel"), "der Notbehelf steht drin: {note}");

        // A partial overlap is a routing somebody chose; it really does sum, and says so.
        let odd = BusRouting::new(BusOutput::pair(0), BusOutput::pair(1));
        assert!(odd.overlaps());
        assert!(!odd.collapsed());
        let note = routing_note(&odd, 4).expect("Ueberschneidung wird erklaert");
        assert!(note.contains("teilweise"), "{note}");

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
}
