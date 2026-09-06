//! From a user action to timed engine commands - the part that needs no audio device.
//!
//! This is the one place where a key press or a button becomes a set of commands with sample
//! positions on them. Both front ends use it: the CLI's `live` subcommand and the desktop app's
//! host thread. It used to exist twice, once on each side, which meant every rule had to be
//! changed twice and only one copy was under test.
//!
//! Three ideas carry the whole file:
//!
//! * **Everything musical happens on a grid point.** A command is stamped with an absolute sample
//!   position and the audio thread executes it exactly there, so it does not matter how long the
//!   command takes to get there - as long as it arrives before its own time.
//! * **`guard` is what makes that true.** Every scheduled position is at least `guard` samples
//!   ahead of where the engine is estimated to be, which is comfortably more than one round
//!   through the callbacks.
//! * **Which grid is [`Quantize`].** The bar grid is fine for corrections; for starting a take it
//!   is the wrong unit, and the reason is in the next section.
//!
//! # Why the loop grid is the default
//!
//! With bar quantisation and an eight-bar loop, a recording pressed in bar 3 starts in bar 4 and
//! then runs eight bars from *there* - across the loop the rest of the session is counting in. To
//! get a take that lines up, the musician has to wait until bar 8 is on the display, hit the key
//! inside that last bar, and have the instrument back in playing position before the downbeat. That
//! is the complaint this module answers.
//!
//! With [`Quantize::Loop`] the same key press in bar 3 arms the take for the start of bar 9: press
//! early, then take six bars to get ready. The take is on the loop grid either way, so it can never
//! straddle a loop boundary.
//!
//! # Which grid a take snaps to
//!
//! There are two rasters, and the choice between them is not a preference but a consequence of how
//! a track stores its layers (`track.rs`): every layer is addressed as
//! `(position - origin) mod loop_len`.
//!
//! * **Track has a loop** (`loop_len > 0`): its own grid, `origin + n * loop_len`, is the only
//!   correct answer for a further layer. The global bar grid would generally miss it - bar
//!   boundaries are rounded individually, so `bar_start(origin_bar + n * bars)` and
//!   `origin + n * loop_len` can differ by a sample or two, and at an odd time signature they
//!   differ more often than not. A layer starting one sample off would still *mix* correctly (the
//!   modulo takes care of that), but the musician would be playing against a loop whose top is not
//!   where he was told it is, and every following overdub would inherit the offset.
//! * **Track is empty**: there is no `origin` yet, so the raster is counted from the start of the
//!   engine - bar 0, bar `bars`, bar `2*bars`, the same grid the click has been marking out since
//!   the session began. That makes the first take of every track land on the same boundaries, which
//!   is what lets two tracks recorded minutes apart play as one loop.
//!
//! A *new* loop (`r`) always uses the global grid, even on a track that already holds one: it
//! throws the old geometry away and defines a new `origin`, and the global grid is what the click
//! and every other track are following.
//!
//! # What stays on the bar grid, and why
//!
//! Stop and playback keep bar quantisation in both modes. They are corrections, not takes: "stop
//! this" that waits up to eight bars reads as a broken button, and a musician reaching for it wants
//! it to act. Playback in particular loses nothing by it - a loop is read as
//! `(position - origin) mod loop_len`, so switching it on mid-loop resumes at the phase the loop is
//! in rather than at a random offset. Only a take, which has to *fill* a loop from its top, needs
//! the loop grid.

use super::command::{Command, MAX_TRACKS};
use super::timeline::Timeline;
use super::track::TrackState;

/// Which grid a take is quantised to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Quantize {
    /// Next bar boundary. What the looper did before there was a choice.
    Bar,
    /// Next loop boundary, so one early press is enough. The default, see the module comment.
    #[default]
    Loop,
}

impl Quantize {
    /// German word for the display.
    pub fn label(self) -> &'static str {
        match self {
            Quantize::Bar => "Takt",
            Quantize::Loop => "Loop",
        }
    }

    /// One German sentence explaining what this mode does to a key press.
    pub fn explanation(self) -> &'static str {
        match self {
            Quantize::Bar => "Aufnahme beginnt an der naechsten Taktgrenze.",
            Quantize::Loop => "Aufnahme beginnt am naechsten Loop-Anfang.",
        }
    }
}

/// What a track is waiting for. Used for the count-in display, not for the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingKind {
    Record,
    Overdub,
    Play,
    Stop,
}

impl PendingKind {
    /// German word for the display, in the same spirit as [`TrackState::label`].
    pub fn label(self) -> &'static str {
        match self {
            PendingKind::Record => "Aufnahme",
            PendingKind::Overdub => "Overdub",
            PendingKind::Play => "Wiedergabe",
            PendingKind::Stop => "Stopp",
        }
    }
}

/// One scheduled action of a track, remembered so the display can count down to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    kind: PendingKind,
    /// Absolute sample position the action takes effect at.
    at: u64,
}

/// How far a track still is from its scheduled action.
///
/// `bars` and `beats` are what is left of the way there, split at the bar line: five bars and two
/// beats means the action happens two beats into the sixth bar from now. Both are zero and `kind`
/// is `None` when nothing is scheduled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lead {
    pub kind: Option<PendingKind>,
    pub bars: u32,
    pub beats: u32,
}

impl Lead {
    /// Whether anything is scheduled at all.
    pub fn is_pending(&self) -> bool {
        self.kind.is_some()
    }

    /// German text for a terminal or a button: `Aufnahme in 5 Takten`, `Overdub in 2 Schlaegen`.
    pub fn text(&self) -> Option<String> {
        let kind = self.kind?;
        Some(match (self.bars, self.beats) {
            (0, 0) => format!("{} jetzt", kind.label()),
            (0, 1) => format!("{} in 1 Schlag", kind.label()),
            (0, b) => format!("{} in {b} Schlaegen", kind.label()),
            (1, 0) => format!("{} in 1 Takt", kind.label()),
            (t, 0) => format!("{} in {t} Takten", kind.label()),
            (1, b) => format!("{} in 1 Takt und {b} Schlaegen", kind.label()),
            (t, b) => format!("{} in {t} Takten und {b} Schlaegen", kind.label()),
        })
    }
}

/// Commands to send, plus the German sentence describing what was scheduled.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheduled {
    pub commands: Vec<Command>,
    pub message: String,
}

/// Turns actions into commands for one engine configuration.
#[derive(Debug, Clone, Copy)]
pub struct Scheduler {
    pub timeline: Timeline,
    /// Configured loop length in bars.
    pub bars: u32,
    /// Head start every scheduled command gets, so it can never arrive after its own time.
    pub guard: u64,
    /// Which grid a take snaps to.
    pub quantize: Quantize,
    /// What each track is waiting for, so the display can count down. Entries expire on their own
    /// once the engine has passed them, so nothing has to acknowledge them.
    pending: [Option<Pending>; MAX_TRACKS],
}

impl Scheduler {
    /// `guard` is four buffers plus 20 ms, the same value the CLI always used: more than one round
    /// through both callbacks, so a command is always in the waiting room before it is due.
    pub fn new(timeline: Timeline, bars: u32, buffer_frames: u32, quantize: Quantize) -> Self {
        Self {
            timeline,
            bars,
            guard: (buffer_frames * 4 + timeline.sample_rate() / 50) as u64,
            quantize,
            pending: [None; MAX_TRACKS],
        }
    }

    /// Where the audio thread is now, as well as the control side can know: the position of the
    /// newest status snapshot plus the time that has passed since it arrived.
    ///
    /// Both streams run on the interface clock, so this is good to a few samples - and the rest is
    /// absorbed by `guard` and by quantising to a grid point.
    pub fn estimated_pos(&self, last: Option<(u64, std::time::Duration)>) -> u64 {
        match last {
            Some((pos, since)) => {
                pos + (since.as_secs_f64() * self.timeline.sample_rate() as f64) as u64
            }
            None => 0,
        }
    }

    /// Next bar boundary far enough ahead for the audio thread to still see the command.
    pub fn next_bar(&self, est: u64) -> u64 {
        self.timeline.bar_start_at_or_after(est + self.guard)
    }

    /// Position for a command that has no musical meaning and should act as soon as possible.
    pub fn soon(&self, est: u64) -> u64 {
        est + self.guard
    }

    /// Where a take may begin: the next point of the configured grid that the audio thread can
    /// still be told about in time.
    ///
    /// `loop_grid` is the track's own `(origin, loop_len)` when it has one. See the module comment
    /// for why that beats the global grid whenever it exists.
    pub fn next_start(&self, est: u64, loop_grid: Option<(u64, u64)>) -> u64 {
        let earliest = est + self.guard;
        match self.quantize {
            Quantize::Bar => self.timeline.bar_start_at_or_after(earliest),
            Quantize::Loop => match loop_grid {
                Some((origin, loop_len)) if loop_len > 0 => {
                    if earliest <= origin {
                        origin
                    } else {
                        // Whole passes since the origin, rounded up: the next top of *this* loop.
                        let passes = (earliest - origin).div_ceil(loop_len);
                        origin + passes * loop_len
                    }
                }
                _ => self.timeline.loop_start_at_or_after(earliest, self.bars),
            },
        }
    }

    /// Remember what a track is waiting for, so [`Scheduler::lead`] can count down to it.
    fn arm(&mut self, track: usize, kind: PendingKind, at: u64) {
        if let Some(slot) = self.pending.get_mut(track) {
            *slot = Some(Pending { kind, at });
        }
    }

    fn disarm(&mut self, track: usize) {
        if let Some(slot) = self.pending.get_mut(track) {
            *slot = None;
        }
    }

    /// How far track `track` still is from its scheduled action, measured from `pos`.
    ///
    /// The distance is counted in whole beats between the beat `pos` is in and the beat the action
    /// falls on, then split at the bar line. That is the same arithmetic the timeline uses for the
    /// display, so the countdown reaches zero on exactly the beat the engine acts on.
    pub fn lead(&self, track: usize, pos: u64) -> Lead {
        let Some(Some(pending)) = self.pending.get(track).copied() else {
            return Lead::default();
        };
        if pos >= pending.at {
            return Lead::default();
        }
        let beats_per_bar = self.timeline.beats_per_bar().max(1);
        let left = self
            .timeline
            .beat_index_at(pending.at)
            .saturating_sub(self.timeline.beat_index_at(pos));
        Lead {
            kind: Some(pending.kind),
            bars: (left / beats_per_bar) as u32,
            beats: (left % beats_per_bar) as u32,
        }
    }

    /// A new loop: replaces whatever the track holds, over the configured number of bars.
    ///
    /// Always the global grid - the take defines a new `origin`, so there is no old geometry worth
    /// aligning to.
    pub fn record(&mut self, track: usize, name: &str, est: u64) -> Scheduled {
        let start = self.next_start(est, None);
        let bar = self.timeline.bar_index_at(start);
        let end = self.timeline.position_of(bar + self.bars as u64, 0);
        self.arm(track, PendingKind::Record, start);
        Scheduled {
            commands: vec![
                Command::StartRecord { track, at: start },
                Command::StopRecord { track, at: end },
                Command::StartPlay { track, at: end },
            ],
            message: format!(
                "\"{name}\": neuer Loop ab Takt {} ueber {} Takte.",
                bar + 1,
                self.bars
            ),
        }
    }

    /// A further layer. It covers exactly one pass of the existing loop and starts on that loop's
    /// own grid; on an empty track it is the first take and gets the configured number of bars, so
    /// an overdub can never fail just because nothing is there yet.
    pub fn overdub(
        &mut self,
        track: usize,
        name: &str,
        est: u64,
        origin: u64,
        loop_len: u64,
        layers: u8,
    ) -> Scheduled {
        let start = self.next_start(est, Some((origin, loop_len)));
        let bar = self.timeline.bar_index_at(start);
        let end = if loop_len > 0 {
            start + loop_len
        } else {
            self.timeline.position_of(bar + self.bars as u64, 0)
        };
        self.arm(track, PendingKind::Overdub, start);
        Scheduled {
            commands: vec![
                Command::StartOverdub { track, at: start },
                Command::StopRecord { track, at: end },
                Command::StartPlay { track, at: end },
            ],
            message: format!("\"{name}\": Ebene {} ab Takt {}.", layers as usize + 1, bar + 1),
        }
    }

    /// One button for three situations, exactly as on the keyboard: a scheduled recording is
    /// cancelled, a running one is closed on the next bar, and otherwise playback stops.
    ///
    /// Bar grid on purpose, in both modes - see the module comment.
    pub fn stop(&mut self, track: usize, name: &str, est: u64, state: TrackState) -> Scheduled {
        match state {
            TrackState::Armed => {
                self.disarm(track);
                Scheduled {
                    commands: vec![Command::ClearTrack {
                        track,
                        at: self.soon(est),
                    }],
                    message: "Geplante Aufnahme abgebrochen.".to_string(),
                }
            }
            TrackState::Recording | TrackState::Overdub => {
                let end = self.next_bar(est);
                self.arm(track, PendingKind::Stop, end);
                Scheduled {
                    commands: vec![
                        Command::StopRecord { track, at: end },
                        Command::StartPlay { track, at: end },
                    ],
                    message: format!("Aufnahme endet mit Takt {}.", self.timeline.bar_index_at(end)),
                }
            }
            _ => {
                self.disarm(track);
                Scheduled {
                    commands: vec![Command::StopPlay {
                        track,
                        at: self.soon(est),
                    }],
                    message: format!("\"{name}\": Wiedergabe gestoppt."),
                }
            }
        }
    }

    pub fn play(&mut self, track: usize, name: &str, est: u64) -> Scheduled {
        let at = self.next_bar(est);
        self.arm(track, PendingKind::Play, at);
        Scheduled {
            commands: vec![Command::StartPlay { track, at }],
            message: format!(
                "\"{name}\": Wiedergabe ab Takt {}.",
                self.timeline.bar_index_at(at) + 1
            ),
        }
    }

    pub fn clear_track(&mut self, track: usize, name: &str, est: u64) -> Scheduled {
        self.disarm(track);
        Scheduled {
            commands: vec![Command::ClearTrack {
                track,
                at: self.soon(est),
            }],
            message: format!("\"{name}\" geleert."),
        }
    }

    pub fn clear_all(&mut self, est: u64) -> Scheduled {
        self.pending = [None; MAX_TRACKS];
        Scheduled {
            commands: vec![Command::ClearAll { at: self.soon(est) }],
            message: "Alles geleert - Zustand wie frisch gestartet.".to_string(),
        }
    }

    /// Switch the grid while the engine runs. Only takes scheduled from now on are affected; one
    /// that is already armed keeps the position it was given, because the engine already has it.
    pub fn set_quantize(&mut self, quantize: Quantize) -> String {
        self.quantize = quantize;
        format!(
            "Quantisierung: {} - {}",
            quantize.label(),
            quantize.explanation()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timeline::TimeSignature;

    const RATE: u32 = 48_000;
    /// 120 BPM, 4/4, 48 kHz: 24000 samples per beat, 96000 per bar - all integers, so every
    /// expectation below can be written down by hand.
    const BAR: u64 = 96_000;

    fn scheduler(bpm: f64, bars: u32, quantize: Quantize) -> Scheduler {
        Scheduler::new(
            Timeline::new(RATE, bpm, TimeSignature::new(4, 4)),
            bars,
            128,
            quantize,
        )
    }

    fn start_of(plan: &Scheduled) -> u64 {
        plan.commands[0].at().expect("das erste Kommando ist getimt")
    }

    #[test]
    fn the_head_start_is_four_buffers_plus_twenty_milliseconds() {
        assert_eq!(scheduler(120.0, 8, Quantize::Loop).guard, 128 * 4 + 48_000 / 50);
    }

    #[test]
    fn the_loop_grid_is_the_default() {
        assert_eq!(Quantize::default(), Quantize::Loop);
    }

    /// The old behaviour, unchanged: everything musical lands on the next bar boundary that is
    /// still far enough ahead for the head start to fit.
    #[test]
    fn with_bar_quantisation_a_recording_starts_on_the_next_bar_as_before() {
        let mut s = scheduler(120.0, 8, Quantize::Bar);
        let plan = s.record(0, "gitarre", 100_000);
        assert_eq!(
            plan.commands,
            vec![
                Command::StartRecord { track: 0, at: 2 * BAR },
                Command::StopRecord { track: 0, at: 2 * BAR + 8 * BAR },
                Command::StartPlay { track: 0, at: 2 * BAR + 8 * BAR },
            ]
        );
        assert!(plan.message.contains("Takt 3"), "{}", plan.message);
        assert!(plan.message.contains("8 Takte"), "{}", plan.message);

        // Exactly on a boundary the *next* one is taken, because the head start has to fit.
        let plan = s.record(1, "stimme", BAR);
        assert_eq!(start_of(&plan), 2 * BAR);
    }

    /// The complaint from the report, as an assertion: at eight bars a key pressed anywhere inside
    /// bars 1 to 8 arms the take for bar 9, so one early press is enough.
    #[test]
    fn with_loop_quantisation_every_bar_of_the_loop_collects_on_the_next_loop_start() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        // One-based bar 3 (index 2) and bar 8 (index 7), both mid-bar.
        for bar in [2u64, 7] {
            let plan = s.record(0, "gitarre", bar * BAR + 1_000);
            assert_eq!(start_of(&plan), 8 * BAR, "Takt {} gehoert zu Loop 1", bar + 1);
            assert_eq!(
                plan.commands[1].at(),
                Some(16 * BAR),
                "die Aufnahme dauert genau einen Loop"
            );
        }
        // Bar 1 of the second loop already belongs to the next one.
        let plan = s.record(0, "gitarre", 8 * BAR + 1_000);
        assert_eq!(start_of(&plan), 16 * BAR);
    }

    /// A press that arrives before a loop boundary but close enough that the head start still fits
    /// must land on *that* boundary, not on the one after it.
    #[test]
    fn a_command_on_a_loop_boundary_stays_on_that_boundary() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        // `next_start` quantises `est + guard`, so aim the head start exactly at the boundary.
        let est = 8 * BAR - s.guard;
        assert_eq!(s.next_start(est, None), 8 * BAR);
        let plan = s.record(0, "gitarre", est);
        assert_eq!(start_of(&plan), 8 * BAR);
        // One sample later the head start no longer fits and the next loop is taken.
        assert_eq!(s.next_start(est + 1, None), 16 * BAR);
    }

    /// An overdub is one pass of the existing loop, never the configured bar count - otherwise a
    /// layer would be longer than what it is layered onto.
    #[test]
    fn an_overdub_covers_exactly_one_pass_of_the_existing_loop() {
        let mut s = scheduler(120.0, 8, Quantize::Bar);
        let loop_len = 4 * BAR;
        let plan = s.overdub(0, "gitarre", 100_000, 0, loop_len, 2);
        assert_eq!(
            plan.commands,
            vec![
                Command::StartOverdub { track: 0, at: 2 * BAR },
                Command::StopRecord { track: 0, at: 2 * BAR + loop_len },
                Command::StartPlay { track: 0, at: 2 * BAR + loop_len },
            ]
        );
        assert!(plan.message.contains("Ebene 3"), "{}", plan.message);

        // On an empty track it behaves like a first take of the configured length.
        let plan = s.overdub(0, "gitarre", 100_000, 0, 0, 0);
        assert_eq!(
            plan.commands[1],
            Command::StopRecord { track: 0, at: 2 * BAR + 8 * BAR }
        );
    }

    /// The property the layer stack rests on: a further layer starts exactly on the existing
    /// loop's own grid, `origin + n * loop_len`, and covers exactly one pass. Checked at time
    /// signatures whose bar length is not a whole number of samples, because that is where a grid
    /// derived from bar boundaries would drift away from the track's own.
    #[test]
    fn an_overdub_lands_on_the_existing_loops_own_grid_at_any_time_signature() {
        for (bpm, bpb, unit, bars) in [
            (120.0, 4, 4, 8u32),
            (100.0, 3, 4, 4),
            (137.0, 7, 8, 5),
            (93.5, 5, 4, 3),
        ] {
            let timeline = Timeline::new(RATE, bpm, TimeSignature::new(bpb, unit));
            let mut s = Scheduler::new(timeline, bars, 128, Quantize::Loop);
            // A loop as the first take would have produced it: origin on the loop grid, length the
            // span of `bars` bars from there.
            let origin = timeline.loop_start_at_or_after(0, bars);
            let loop_len = timeline.span_bars(timeline.bar_index_at(origin), bars);

            for pass in 0..12u64 {
                for offset in [1u64, 1_000, loop_len / 3, loop_len - 1] {
                    let est = origin + pass * loop_len + offset;
                    let plan = s.overdub(0, "t", est, origin, loop_len, 1);
                    let start = start_of(&plan);
                    assert!(start >= est + s.guard, "der Vorlauf muss passen");
                    assert_eq!(
                        (start - origin) % loop_len,
                        0,
                        "{bpb}/{unit} bei {bpm}: Ebene liegt sample-genau auf dem Loop-Raster"
                    );
                    assert_eq!(
                        plan.commands[1].at(),
                        Some(start + loop_len),
                        "eine Ebene ist genau einen Durchlauf lang"
                    );
                }
            }
        }
    }

    /// With bar quantisation an overdub keeps the old behaviour: next bar, whatever the loop grid
    /// would say.
    #[test]
    fn bar_quantisation_ignores_the_loop_grid() {
        let mut s = scheduler(120.0, 8, Quantize::Bar);
        // A loop whose origin is deliberately not a multiple of a bar.
        let plan = s.overdub(0, "t", 100_000, 5_000, 8 * BAR, 1);
        assert_eq!(start_of(&plan), 2 * BAR, "die Taktgrenze gewinnt");
    }

    #[test]
    fn stopping_means_three_different_things_depending_on_the_state() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let armed = s.stop(0, "gitarre", 100_000, TrackState::Armed);
        assert!(matches!(armed.commands[0], Command::ClearTrack { .. }));
        assert!(armed.message.contains("abgebrochen"), "{}", armed.message);

        let recording = s.stop(0, "gitarre", 100_000, TrackState::Recording);
        assert_eq!(
            recording.commands,
            vec![
                Command::StopRecord { track: 0, at: 2 * BAR },
                Command::StartPlay { track: 0, at: 2 * BAR },
            ]
        );

        let playing = s.stop(0, "gitarre", 100_000, TrackState::Playing);
        assert!(matches!(playing.commands[0], Command::StopPlay { .. }));
        // Untimed-feeling actions still carry a timestamp, just not a quantised one.
        assert_eq!(playing.commands[0].at(), Some(100_000 + s.guard));
    }

    /// Stop and playback stay on the bar grid even in loop mode - a correction that waits eight
    /// bars is a broken button.
    #[test]
    fn stop_and_playback_stay_on_the_bar_grid_in_loop_mode() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.play(0, "gitarre", 100_000);
        assert_eq!(start_of(&plan), 2 * BAR);
        let plan = s.stop(0, "gitarre", 100_000, TrackState::Recording);
        assert_eq!(start_of(&plan), 2 * BAR);
    }

    #[test]
    fn the_position_estimate_follows_the_interface_clock() {
        let s = scheduler(120.0, 8, Quantize::Loop);
        assert_eq!(s.estimated_pos(None), 0);
        assert_eq!(
            s.estimated_pos(Some((48_000, std::time::Duration::from_millis(100)))),
            48_000 + 4_800
        );
    }

    /// A fractional beat length is the case that catches an incrementally advanced grid; the plan
    /// must still land on positions the timeline itself computes.
    #[test]
    fn odd_tempos_and_time_signatures_stay_on_the_timeline_grid() {
        let timeline = Timeline::new(RATE, 137.0, TimeSignature::new(7, 8));
        let mut s = Scheduler::new(timeline, 5, 128, Quantize::Bar);
        let plan = s.record(0, "t", 1_000_000);
        let start = start_of(&plan);
        let bar = timeline.bar_index_at(start);
        assert_eq!(start, timeline.bar_start(bar), "Start liegt auf einer Taktgrenze");
        assert_eq!(
            plan.commands[1].at(),
            Some(timeline.position_of(bar + 5, 0)),
            "Ende liegt fuenf Takte spaeter, absolut gerechnet"
        );
    }

    // ---- the count-in --------------------------------------------------------------------

    /// The number on screen has to mean what it says: reading it off at `pos` and walking that
    /// many bars and beats forward has to arrive at the sample the engine acts on.
    #[test]
    fn the_lead_counts_down_to_exactly_the_scheduled_sample() {
        for (bpm, bpb, unit, bars) in [(120.0, 4, 4, 8u32), (137.0, 7, 8, 5), (100.0, 3, 4, 4)] {
            let timeline = Timeline::new(RATE, bpm, TimeSignature::new(bpb, unit));
            let mut s = Scheduler::new(timeline, bars, 128, Quantize::Loop);
            let est = timeline.bar_start(2) + 1_234;
            let plan = s.record(0, "t", est);
            let at = start_of(&plan);

            // Walk from just after the press to the scheduled sample, one beat at a time.
            let first_beat = timeline.beat_index_at(est);
            let last_beat = timeline.beat_index_at(at);
            for beat in first_beat..last_beat {
                let pos = timeline.beat_start(beat);
                let lead = s.lead(0, pos);
                assert_eq!(lead.kind, Some(PendingKind::Record));
                let walked = lead.bars as u64 * bpb as u64 + lead.beats as u64;
                assert_eq!(
                    timeline.beat_start(beat + walked),
                    at,
                    "{bpb}/{unit}: {} Takte und {} Schlaege muessen genau auf den Start fuehren",
                    lead.bars,
                    lead.beats
                );
            }
            // On the scheduled sample itself nothing is pending any more.
            assert_eq!(s.lead(0, at), Lead::default());
            assert!(!s.lead(0, at).is_pending());
        }
    }

    /// The last bar is counted down in beats, which is what the display switches on.
    #[test]
    fn the_lead_turns_into_beats_inside_the_last_bar() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.record(0, "gitarre", 1_000);
        assert_eq!(start_of(&plan), 8 * BAR);

        assert_eq!(
            s.lead(0, 0),
            Lead {
                kind: Some(PendingKind::Record),
                bars: 8,
                beats: 0
            }
        );
        assert_eq!(s.lead(0, 0).text().unwrap(), "Aufnahme in 8 Takten");
        // Three beats into the last bar of the count-in: one beat left.
        let pos = 7 * BAR + 3 * 24_000;
        assert_eq!(
            s.lead(0, pos),
            Lead {
                kind: Some(PendingKind::Record),
                bars: 0,
                beats: 1
            }
        );
        assert_eq!(s.lead(0, pos).text().unwrap(), "Aufnahme in 1 Schlag");
        assert_eq!(s.lead(0, 6 * BAR).text().unwrap(), "Aufnahme in 2 Takten");
        assert_eq!(s.lead(0, 7 * BAR).text().unwrap(), "Aufnahme in 1 Takt");
    }

    /// Every action that can be waited for announces itself, and everything that cancels one takes
    /// the announcement away again.
    #[test]
    fn the_lead_follows_what_was_scheduled_and_disappears_when_it_is_cancelled() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        assert_eq!(s.lead(0, 0), Lead::default(), "frisch ist nichts geplant");

        s.overdub(0, "t", 1_000, 0, 8 * BAR, 1);
        assert_eq!(s.lead(0, 1_000).kind, Some(PendingKind::Overdub));

        s.play(1, "t", 1_000);
        assert_eq!(s.lead(1, 1_000).kind, Some(PendingKind::Play));

        s.stop(1, "t", 1_000, TrackState::Recording);
        assert_eq!(s.lead(1, 1_000).kind, Some(PendingKind::Stop));

        // Cancelling an armed take, clearing a track and clearing everything all take it back.
        s.stop(0, "t", 1_000, TrackState::Armed);
        assert_eq!(s.lead(0, 1_000), Lead::default());
        s.record(0, "t", 1_000);
        s.clear_track(0, "t", 1_000);
        assert_eq!(s.lead(0, 1_000), Lead::default());
        s.record(0, "t", 1_000);
        s.record(1, "t", 1_000);
        s.clear_all(1_000);
        assert_eq!(s.lead(0, 1_000), Lead::default());
        assert_eq!(s.lead(1, 1_000), Lead::default());
        // A track index beyond the ceiling must not panic, only report nothing.
        assert_eq!(s.lead(MAX_TRACKS + 3, 1_000), Lead::default());
    }

    #[test]
    fn switching_the_grid_at_runtime_changes_the_next_take_only() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.record(0, "t", 1_000);
        assert_eq!(start_of(&plan), 8 * BAR);
        let message = s.set_quantize(Quantize::Bar);
        assert!(message.contains("Takt"), "{message}");
        // The armed take keeps its position; the engine already has that command.
        assert_eq!(s.lead(0, 0).bars, 8);
        let plan = s.record(1, "t", 1_000);
        assert_eq!(start_of(&plan), BAR, "der naechste Take nimmt das neue Raster");
    }
}
