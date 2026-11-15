//! Phase 3, second half: the runner - the piece that actually plays a compiled score.
//!
//! ```text
//!  CompiledScore ──► Runner ──► Scheduler ──► Command{at} ──► Audio-Thread
//!   (Sollzustaende)   (Ist gegen Soll)   (Zeitstempel)
//! ```
//!
//! It runs in the **control thread** and never touches engine state directly: everything it does
//! leaves as a [`Command`] with a musical timestamp on it. The runner does not wait for bar lines -
//! it works out the sample a change falls on and hands the commands over long before that sample
//! arrives.
//!
//! # The declarative model, and why it means *fewer* commands
//!
//! A section states the complete target state of **every** track, not a delta. The runner therefore
//! compares what it last asked of a track with what the next section asks, and sends only the
//! difference. Written out, because it is the whole point:
//!
//! | vorher | jetzt | Kommandos |
//! |---|---|---|
//! | egal | `record` | `StartRecord`, `StopRecord`, `StartPlay` - ein Take ueber die Sektion |
//! | egal | `overdub` | `StartOverdub`, `StopRecord`, `StartPlay` - eine Ebene, ein Loop-Durchlauf |
//! | klingt schon | `play` | **keins** |
//! | still | `play` | `StartPlay` |
//! | klingt | `stop` / `hear_through` | `StopPlay` |
//! | still | `stop` | **keins** |
//!
//! A track that stands on `play` through five sections gets exactly zero commands, and so does one
//! that stands on `stop`. That is not tidiness: every superfluous command at a section boundary is
//! a state change on the audio thread at a musically exposed moment, and the kind of fault one only
//! notices as a click twenty minutes into a take.
//!
//! # Why the runner predicts the loop geometry instead of reading it back
//!
//! An overdub has to be quantised to the track's own `(origin, loop_len)` grid, and a section is
//! scheduled *before* the take that defines that grid has finished - the status snapshot still says
//! `loop_len == 0` at that moment. The runner does not need the snapshot: it issued the take itself,
//! so it knows the loop will be exactly `[start, end)` of the section it recorded in. Every track
//! therefore carries a predicted geometry, written when the runner schedules a `record`, and the
//! prediction is exact because nothing else sends commands while a score is running.
//!
//! # Why a change never lands inside a running take
//!
//! A release with `quantize: bar` could otherwise fall in bar 3 of an eight-bar recording. Cutting
//! the take there would define a **three-bar loop**, and everything recorded against it afterwards
//! would be wrong - a far worse outcome than waiting five bars. So the change position is pushed to
//! the end of the running take before it is quantised, and the runner says so. The take is finished
//! cleanly and the section changes on the boundary right after it.
//!
//! # Monitoring is the one thing that cannot be timestamped
//!
//! [`Command::SetMonitor`] carries no position (see `command.rs`), so it cannot be sent five bars
//! early - the singer would become audible five bars early. The runner therefore holds the monitor
//! change back and sends it in [`Runner::tick`], at the moment the engine has actually reached the
//! section boundary. That is accurate to one turn of the control loop, a few milliseconds; nothing
//! that is *recorded* depends on it, because a recording is sample-stamped either way.

use std::time::Duration;

use crate::score::{CompiledScore, TrackState as ScoreState};

use super::command::{Command, MAX_TRACKS};
use super::live::TrackDef;
use super::schedule::{Quantize, Scheduled, Scheduler};
use super::timeline::Timeline;

/// Bars of count-in before the first section, unless the caller says otherwise.
///
/// One bar, the way a band is counted in. It is the shortest lead that is still a *bar* - the
/// musician hears a complete bar of the metre he is about to play in, which is what makes the entry
/// hittable; in 3/4 at 141 BPM that is 1.28 seconds. Two bars would be waiting, half a bar would not
/// establish the metre. The lead is never shorter than this: the count-in starts on the next bar
/// line the command queue can still be told about in time, so what the musician gets is the rest of
/// the bar he pressed in *plus* the full count-in.
pub const DEFAULT_COUNT_IN_BARS: u32 = 1;
/// Longest count-in that can be set. More than eight bars of click is waiting, not counting in.
pub const MAX_COUNT_IN_BARS: u32 = 8;

/// The refusal every by-hand transport action gets while a score is playing.
///
/// It lives here, next to the runner whose rule it is, because it has to be **one** sentence for
/// **every** way of pressing the button: the mouse (`Action::moves_transport` in the app's
/// `host.rs`) and a MIDI pad (`midi::Target::moves_transport`) run into the same wall for the same
/// reason, and two wordings would suggest two rules. See `docs/architektur.md` section 10.
pub const TRANSPORT_BELONGS_TO_SCORE: &str =
    "The score is running - it sends the Record and Play commands itself. \
     Manual transport works again after \"Stop all\". Level, pan, layers and \
     effects stay adjustable meanwhile.";

/// What the runner is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Score loaded, nothing scheduled.
    Idle,
    /// The count-in is running; the first section is armed.
    CountIn,
    /// A section is sounding.
    Running,
    /// The last section is over.
    Finished,
    /// The musician stopped everything.
    Stopped,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Idle => "ready",
            Phase::CountIn => "count-in",
            Phase::Running => "running",
            Phase::Finished => "end",
            Phase::Stopped => "stopped",
        }
    }

    /// Whether the score is over, one way or the other.
    pub fn is_over(self) -> bool {
        matches!(self, Phase::Finished | Phase::Stopped)
    }
}

/// One section, with `repeat:` resolved and the track states flattened into track order.
///
/// A copy rather than a borrow of [`crate::score::CompiledSection`]: the runner mutates itself
/// while it reads a section, and a `Vec` indexed by track number is what the diff wants anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SectionPlan {
    id: String,
    bars: u32,
    autorelease: bool,
    quantize: Quantize,
    /// Target state per track, in track order. Always complete - the compiler fills the gaps.
    want: Vec<ScoreState>,
}

/// Where a section sits on the sample timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub index: usize,
    /// First sample of the section.
    pub start: u64,
    /// Zero-based bar index of that sample.
    pub start_bar: u64,
    pub bars: u32,
}

/// What an armed change will turn into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Section(usize),
    /// Past the last section: everything falls silent and the run is over.
    End,
}

/// A change that has been scheduled but has not taken effect yet.
///
/// Its commands are already in the engine's waiting room - that is the point of scheduling early -
/// which is exactly why it cannot be retargeted afterwards. See [`Runner::goto`].
#[derive(Clone, Debug, PartialEq)]
struct Armed {
    target: Target,
    at: u64,
    at_bar: u64,
    /// Tracks this change starts a take on. Needed only by [`Runner::stop_all`], which has to
    /// neutralise commands the audio thread already holds.
    takes: Vec<usize>,
}

/// The runner's own picture of one track.
#[derive(Clone, Debug)]
struct TrackRun {
    name: String,
    /// Whether the score allows this track to be monitored at all.
    can_monitor: bool,
    /// An instrument track: heard whenever it is played (unless the score says monitor: false,
    /// which the engine was started with), so the sections never switch its monitoring.
    instrument: bool,
    /// What the last scheduled section asks of it.
    target: ScoreState,
    /// Whether the runner believes the track is audible once that section has begun.
    sounding: bool,
    /// Predicted loop geometry - see the module comment. `len == 0` means "no loop yet".
    origin: u64,
    len: u64,
    /// The same length in bars. Kept beside `len` because bar boundaries are rounded individually:
    /// eight bars here and eight bars there can differ by a sample, so the question "is this
    /// section long enough for one pass of the loop" has to be asked in bars, not in samples.
    len_bars: u32,
    layers: u8,
    /// End of a take the runner has scheduled and that has not been passed yet; 0 means none.
    take_end: u64,
    /// The bar that end falls in. A take's end and a section's bar line can differ by a sample
    /// (every bar boundary is rounded on its own), and a change must not be pushed a whole section
    /// further just because of that - so "is a take still running here" is asked in bars.
    take_end_bar: u64,
    /// Monitoring as last sent to the engine.
    monitor: bool,
}

/// What a display needs, as one snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct RunnerView {
    pub phase: Phase,
    /// Section sounding right now, 0-based, and its id.
    pub section: Option<usize>,
    pub section_id: String,
    pub section_count: usize,
    pub bars_total: u32,
    /// 1-based bar inside the current pass of the section.
    pub bar: u32,
    /// 1-based beat inside that bar.
    pub beat: u32,
    /// 1-based pass number - a section without `autorelease` repeats until the release button.
    pub pass: u32,
    /// Target state per track, in track order.
    pub tracks: Vec<ScoreState>,
    /// Description of an armed change plus its lead, or `None`.
    pub armed: Option<ArmedView>,
    /// Bars of count-in the next start plays.
    pub count_in_bars: u32,
}

/// An armed change as the display shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmedView {
    /// `voice_2` or `Ende`.
    pub label: String,
    /// Index of the section the change leads to, or `None` when it leads past the last one. A
    /// display needs the number rather than the name: two sections may carry the same id, and a
    /// block preview highlights a card by position.
    pub section: Option<usize>,
    pub bars: u32,
    pub beats: u32,
    /// Whether this is the count-in rather than a section change.
    pub count_in: bool,
}

impl ArmedView {
    /// `Switch armed: "voice_2" in 5 bars`, or the count-in equivalent.
    pub fn text(&self) -> String {
        let head = if self.count_in {
            "Count-in: start".to_string()
        } else {
            format!("Switch armed: \"{}\"", self.label)
        };
        format!("{head} {}", lead_text(self.bars, self.beats))
    }
}

/// Plays a compiled score against one engine configuration.
#[derive(Debug)]
pub struct Runner {
    score: CompiledScore,
    plans: Vec<SectionPlan>,
    sched: Scheduler,
    count_in_bars: u32,
    phase: Phase,
    tracks: Vec<TrackRun>,
    current: Option<Placement>,
    armed: Option<Armed>,
    /// Commands waiting to be handed to the engine, oldest first.
    out: Vec<Command>,
    /// Latest sentence for the display.
    message: String,
    /// Sentences the runner produced on its own, at a section boundary - nobody pressed anything,
    /// so no command reply carries them. Collected until the caller takes them.
    notices: Vec<String>,
}

impl Runner {
    /// Build a runner for `score` on an engine that runs at `timeline` with `buffer_frames`.
    ///
    /// The scheduler's loop length is the **longest** section of the score, which is also the length
    /// every layer buffer has to be allocated at.
    pub fn new(score: CompiledScore, timeline: Timeline, buffer_frames: u32) -> Result<Self, String> {
        if score.sections.is_empty() {
            return Err("The score has no sections.".to_string());
        }
        if score.tracks.len() > MAX_TRACKS {
            return Err(format!(
                "The score names {} tracks; the engine can run at most {MAX_TRACKS}.",
                score.tracks.len()
            ));
        }
        let plans: Vec<SectionPlan> = score
            .sections
            .iter()
            .map(|section| SectionPlan {
                id: section.id.clone(),
                bars: section.bars.max(1),
                autorelease: section.autorelease,
                quantize: section.quantize,
                want: score
                    .tracks
                    .iter()
                    .map(|track| {
                        section
                            .tracks
                            .get(&track.name)
                            .copied()
                            .unwrap_or(ScoreState::Stop)
                    })
                    .collect(),
            })
            .collect();
        let tracks: Vec<TrackRun> = score
            .tracks
            .iter()
            .map(|track| TrackRun {
                name: track.name.clone(),
                can_monitor: track.monitor,
                instrument: track.is_instrument(),
                // Before anything is scheduled every track is silent and empty, which is exactly
                // the state a freshly started engine is in.
                target: ScoreState::Stop,
                sounding: false,
                origin: 0,
                len: 0,
                len_bars: 0,
                layers: 0,
                take_end: 0,
                take_end_bar: 0,
                monitor: false,
            })
            .collect();
        let bars = plans.iter().map(|p| p.bars).max().unwrap_or(1);
        Ok(Self {
            score,
            plans,
            // The quantisation of the scheduler itself is never used by the runner - every section
            // brings its own - so the default is only what the shared display strings read.
            sched: Scheduler::new(timeline, bars, buffer_frames, Quantize::Loop),
            count_in_bars: DEFAULT_COUNT_IN_BARS,
            phase: Phase::Idle,
            tracks,
            current: None,
            armed: None,
            out: Vec::new(),
            message: String::new(),
            notices: Vec::new(),
        })
    }

    /// Bars of count-in before the first section. See [`DEFAULT_COUNT_IN_BARS`].
    #[must_use]
    pub fn with_count_in(mut self, bars: u32) -> Self {
        self.count_in_bars = bars.clamp(1, MAX_COUNT_IN_BARS);
        self
    }

    /// Bars of count-in the next start plays - after the clamp to at least one.
    pub fn count_in_bars(&self) -> u32 {
        self.count_in_bars
    }

    /// Change the count-in of a loaded score before it is started (or after a rewind). Refused
    /// while it plays: the count-in of a running score is already over or already scheduled.
    pub fn set_count_in(&mut self, bars: u32) -> String {
        self.message = if self.phase == Phase::Idle {
            self.count_in_bars = bars.clamp(1, MAX_COUNT_IN_BARS);
            let note = if bars == 0 {
                " (at least one bar - without it the first take would start at the very next bar line)"
            } else if bars > MAX_COUNT_IN_BARS {
                " (at most 8)"
            } else {
                ""
            };
            format!(
                "Count-in: {} bar{}{note}.",
                self.count_in_bars,
                if self.count_in_bars == 1 { "" } else { "s" }
            )
        } else {
            "The count-in can only be changed before the score starts.".to_string()
        };
        self.message.clone()
    }

    pub fn score(&self) -> &CompiledScore {
        &self.score
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.sched
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The longest section in bars - the length every layer buffer has to hold.
    pub fn max_bars(&self) -> u32 {
        self.sched.bars
    }

    /// Latest sentence about what happened.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where the audio thread is now, as well as the control side can know.
    pub fn estimated_pos(&self, last: Option<(u64, Duration)>) -> u64 {
        self.sched.estimated_pos(last)
    }

    /// What happened on its own since the last call: a section began, the score ended. Oldest
    /// first. A front end shows these the way it shows the answer to a button press.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Commands the engine has not been given yet. The caller sends them in this order.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.out)
    }

    // ---- the buttons -------------------------------------------------------------------------

    /// Start the count-in and arm the first section.
    ///
    /// Everything the first section needs is scheduled here, with timestamps - the runner does not
    /// come back at the boundary to do it.
    pub fn start(&mut self, est: u64) -> String {
        if self.phase.is_over() {
            self.message =
                "The score has ended. Use \"Back to start\" to clear the tracks and play it again."
                    .to_string();
            return self.message.clone();
        }
        if self.phase != Phase::Idle {
            self.message = "The score is already running.".to_string();
            return self.message.clone();
        }
        // The count-in begins on the first bar line the audio thread can still be told about in
        // time, so the lead is never shorter than the count-in itself.
        let anchor = self.sched.next_bar(est);
        let anchor_bar = self.sched.timeline.bar_index_at(anchor);
        let first_bar = anchor_bar + self.count_in_bars as u64;
        let first = self.sched.timeline.bar_start(first_bar);
        self.phase = Phase::CountIn;
        self.arm(Target::Section(0), first, first_bar);
        self.message = format!(
            "Count-in: {} bar{} of click, then \"{}\".",
            self.count_in_bars,
            if self.count_in_bars == 1 { "" } else { "s" },
            self.plans[0].id,
        );
        self.message.clone()
    }

    /// The release button: arm the change to the next section.
    ///
    /// Pressing it twice does not skip anything - the second press finds a change already armed and
    /// only reports how far away it is.
    pub fn next(&mut self, est: u64) -> String {
        self.message = match self.phase {
            Phase::Idle => "The score is not running yet - press Start first.".to_string(),
            Phase::Finished | Phase::Stopped => "The score has ended.".to_string(),
            Phase::CountIn | Phase::Running => match self.armed.clone() {
                Some(armed) => {
                    let (bars, beats) = self.sched.distance(est, armed.at);
                    format!(
                        "A switch to \"{}\" is already armed ({}).",
                        self.target_label(armed.target),
                        lead_text(bars, beats)
                    )
                }
                None => self.arm_next(est, None),
            },
        };
        self.message.clone()
    }

    /// Jump to a section instead of taking the next one.
    ///
    /// Refused while a change is armed, and the refusal is the honest answer rather than a
    /// limitation worth hiding: the armed section's commands are already sitting in the audio
    /// thread's waiting room with their timestamps on them, and nothing in this design can take
    /// them back. Retargeting would mean firing both.
    pub fn goto(&mut self, index: usize, est: u64) -> String {
        self.message = if index >= self.plans.len() {
            format!(
                "There is no section {}. Sections: 1 to {}.",
                index + 1,
                self.plans.len()
            )
        } else {
            match self.phase {
                Phase::Idle => "The score is not running yet - press Start first.".to_string(),
                Phase::Finished | Phase::Stopped => "The score has ended.".to_string(),
                Phase::CountIn | Phase::Running => match self.armed.clone() {
                    Some(armed) => format!(
                        "A switch to \"{}\" is already armed - jump again once it has happened.",
                        self.target_label(armed.target)
                    ),
                    None => self.arm_next(est, Some(index)),
                },
            }
        };
        self.message.clone()
    }

    /// Stop every track and end the run.
    pub fn stop_all(&mut self, est: u64) -> String {
        let soon = self.sched.soon(est);
        let bar = self.sched.next_bar(est);
        let mut stopped = 0usize;
        for track in 0..self.tracks.len() {
            let name = self.tracks[track].name.clone();
            // A take that is still running is closed on the bar line rather than left open; the
            // loop it defines is short, but the musician asked for silence.
            let take_end = self.tracks[track].take_end;
            if take_end > bar {
                self.out.push(Command::StopRecord { track, at: bar });
            }
            if self.tracks[track].sounding || take_end != 0 {
                let plan = self.sched.stop_play_at(track, &name, soon);
                self.push(plan);
                stopped += 1;
            }
            // Every take was scheduled as `StartRecord, StopRecord, StartPlay` in one go, and the
            // last two are in the engine already, stamped for the take's planned end. Cutting the
            // take short does not recall them, so the track would start playing again on its own
            // at that sample. A `StopPlay` at the very same position, sent after them, keeps it
            // silent - commands due at one position run in the order they were sent.
            if take_end != 0 {
                self.out.push(Command::StopPlay {
                    track,
                    at: take_end,
                });
            }
            self.set_monitor(track, false);
            self.tracks[track].sounding = false;
            self.tracks[track].target = ScoreState::Stop;
            self.tracks[track].take_end = 0;
        }

        // An armed change cannot be recalled, so it is neutralised where it lands: a track it would
        // have started a take on is cleared at that very sample (which is what `record` was going to
        // do to it anyway), every other one is stopped again.
        let note = match self.armed.take() {
            Some(armed) => {
                for track in 0..self.tracks.len() {
                    if armed.takes.contains(&track) {
                        self.out.push(Command::ClearTrack {
                            track,
                            at: armed.at,
                        });
                    } else {
                        self.out.push(Command::StopPlay {
                            track,
                            at: armed.at,
                        });
                    }
                }
                format!(
                    " The armed switch to \"{}\" was already in the engine; it is silenced where it lands.",
                    self.target_label(armed.target),
                )
            }
            None => String::new(),
        };
        self.phase = Phase::Stopped;
        self.current = None;
        self.message = format!(
            "Stopped all ({stopped} track{}).{note}",
            if stopped == 1 { "" } else { "s" }
        );
        self.message.clone()
    }

    /// Back to the start: the run is forgotten and the score can be started again.
    ///
    /// **The tracks are cleared, and that is not optional.** The runner plans every overdub
    /// against the loop geometry of the takes it scheduled itself and starts from the belief that
    /// every track is empty (see the module comment). A loop left over from the previous run would
    /// be invisible to that plan - the first `play` would not start it, the first `overdub` would
    /// land on a grid the runner does not know. So rewinding means: clear every track (one
    /// `ClearAll`, which also drops whatever the previous run still had waiting in the engine), and
    /// begin again from an empty session, exactly as after loading.
    ///
    /// Refused while the score plays: "Stop all" first. Returns the commands to send; the caller
    /// also has to restore the score's track setup once the clear has happened, because a clear
    /// puts every track back to the values it was *started* with.
    pub fn rewind(&mut self, est: u64) -> Result<(String, Vec<Command>), String> {
        if matches!(self.phase, Phase::CountIn | Phase::Running) {
            return Err(
                "The score is playing. Press \"Stop all\" first, then \"Back to start\"."
                    .to_string(),
            );
        }
        let clear = Command::ClearAll {
            at: self.sched.soon(est),
        };
        for track in self.tracks.iter_mut() {
            track.target = ScoreState::Stop;
            track.sounding = false;
            track.origin = 0;
            track.len = 0;
            track.len_bars = 0;
            track.layers = 0;
            track.take_end = 0;
            track.take_end_bar = 0;
            // A clear restores each track's start-up monitoring, which the caller then switches
            // off again; the runner's picture is "off" either way.
            track.monitor = false;
        }
        self.out.clear();
        self.notices.clear();
        self.current = None;
        self.armed = None;
        self.phase = Phase::Idle;
        self.message = format!(
            "Back to start: all tracks cleared, \"{}\" is ready to play again.",
            self.score.title
        );
        Ok((self.message.clone(), vec![clear]))
    }

    // ---- the clock ---------------------------------------------------------------------------

    /// One turn of the control loop: `pos` is where the engine is.
    ///
    /// This is where an armed change becomes the sounding section, where the monitoring of that
    /// section is switched (the one command that carries no timestamp) and where the follow-up of
    /// an `autorelease` section is armed.
    pub fn tick(&mut self, pos: u64) {
        for track in self.tracks.iter_mut() {
            if track.take_end != 0 && pos >= track.take_end {
                track.take_end = 0;
                track.take_end_bar = 0;
            }
        }
        let Some(armed) = self.armed.clone() else {
            return;
        };
        if pos < armed.at {
            return;
        }
        self.armed = None;
        match armed.target {
            Target::Section(index) => {
                self.current = Some(Placement {
                    index,
                    start: armed.at,
                    start_bar: armed.at_bar,
                    bars: self.plans[index].bars,
                });
                self.phase = Phase::Running;
                self.apply_monitor(index);
                self.message = format!(
                    "Section {}/{} \"{}\": {} bar{}{}.",
                    index + 1,
                    self.plans.len(),
                    self.plans[index].id,
                    self.plans[index].bars,
                    if self.plans[index].bars == 1 { "" } else { "s" },
                    if self.plans[index].autorelease {
                        ""
                    } else {
                        ", loops until Release"
                    }
                );
                self.notices.push(self.message.clone());
                if self.plans[index].autorelease {
                    // The follow-up is known to the sample, so it is armed and sent right away.
                    let at_bar = armed.at_bar + self.plans[index].bars as u64;
                    let at = self.sched.timeline.bar_start(at_bar);
                    let target = if index + 1 < self.plans.len() {
                        Target::Section(index + 1)
                    } else {
                        Target::End
                    };
                    self.arm(target, at, at_bar);
                }
            }
            Target::End => {
                for track in 0..self.tracks.len() {
                    self.set_monitor(track, false);
                }
                self.current = None;
                self.phase = Phase::Finished;
                self.message = "End of the score - all tracks silent.".to_string();
                self.notices.push(self.message.clone());
            }
        }
    }

    // ---- what the display reads --------------------------------------------------------------

    pub fn view(&self, pos: u64) -> RunnerView {
        let timeline = &self.sched.timeline;
        let (section, section_id, bars_total, bar, pass) = match self.current {
            Some(p) => {
                let elapsed = timeline.bar_index_at(pos).saturating_sub(p.start_bar);
                let bars = p.bars.max(1) as u64;
                (
                    Some(p.index),
                    self.plans[p.index].id.clone(),
                    p.bars,
                    (elapsed % bars) as u32 + 1,
                    (elapsed / bars) as u32 + 1,
                )
            }
            None => (None, String::new(), 0, 0, 0),
        };
        let beat = (timeline.beat_index_at(pos) % timeline.beats_per_bar().max(1)) as u32 + 1;
        let tracks = match self.current {
            Some(p) => self.plans[p.index].want.clone(),
            None => match self.armed.as_ref().map(|a| a.target) {
                Some(Target::Section(i)) => self.plans[i].want.clone(),
                _ => vec![ScoreState::Stop; self.tracks.len()],
            },
        };
        let armed = self.armed.as_ref().map(|a| {
            let (bars, beats) = self.sched.distance(pos, a.at);
            ArmedView {
                label: self.target_label(a.target),
                section: match a.target {
                    Target::Section(index) => Some(index),
                    Target::End => None,
                },
                bars,
                beats,
                count_in: self.phase == Phase::CountIn,
            }
        });
        RunnerView {
            phase: self.phase,
            section,
            section_id,
            section_count: self.plans.len(),
            bars_total,
            bar,
            beat,
            pass,
            tracks,
            armed,
            count_in_bars: self.count_in_bars,
        }
    }

    /// Name of a track, for the display.
    pub fn track_name(&self, index: usize) -> &str {
        self.tracks
            .get(index)
            .map(|t| t.name.as_str())
            .unwrap_or("?")
    }

    /// Sample position and bar index an armed change takes effect at, if one is armed.
    pub fn armed_at(&self) -> Option<(u64, u64)> {
        self.armed.as_ref().map(|a| (a.at, a.at_bar))
    }

    /// Section ids in order, for a table of contents.
    pub fn section_ids(&self) -> Vec<&str> {
        self.plans.iter().map(|p| p.id.as_str()).collect()
    }

    // ---- the machinery -----------------------------------------------------------------------

    fn target_label(&self, target: Target) -> String {
        match target {
            Target::Section(i) => self.plans[i].id.clone(),
            Target::End => "End".to_string(),
        }
    }

    fn push(&mut self, plan: Scheduled) {
        self.out.extend(plan.commands);
    }

    /// Monitoring is untimed, so it is only ever changed here - when the engine has reached the
    /// boundary - and only when it really differs from what the engine was last told.
    fn set_monitor(&mut self, track: usize, on: bool) {
        if self.tracks[track].instrument {
            return;
        }
        let on = on && self.tracks[track].can_monitor;
        if self.tracks[track].monitor == on {
            return;
        }
        self.tracks[track].monitor = on;
        self.out.push(Command::SetMonitor { track, on });
    }

    fn apply_monitor(&mut self, index: usize) {
        for track in 0..self.tracks.len() {
            let want = matches!(
                self.plans[index].want[track],
                ScoreState::Record | ScoreState::Overdub | ScoreState::HearThrough
            );
            self.set_monitor(track, want);
        }
    }

    /// Where a change triggered now may take effect, on the grid the current section asks for.
    ///
    /// Two things are added on top of the raw grid: the head start every command needs, and the end
    /// of any take that is still running - see the module comment for why a take is never cut.
    ///
    /// Returns the sample, the bar index, and whether a running take pushed the change further
    /// than the section's own grid would have put it.
    fn change_position(&self, est: u64, current: Placement) -> (u64, u64, bool) {
        // The first bar a change may fall on: no earlier than the bar a running take ends in.
        let min_bar = self
            .tracks
            .iter()
            .filter(|t| t.take_end != 0)
            .map(|t| t.take_end_bar)
            .max()
            .unwrap_or(0);
        let (at, at_bar) = self.grid_position(est, current, min_bar);
        let (_, free_bar) = self.grid_position(est, current, 0);
        (at, at_bar, at_bar > free_bar)
    }

    /// The first point of the current section's quantisation grid at or after `est` plus the head
    /// start, and not before bar `min_bar`.
    fn grid_position(&self, est: u64, current: Placement, min_bar: u64) -> (u64, u64) {
        let timeline = self.sched.timeline;
        let earliest = est + self.sched.guard;
        match self.plans[current.index].quantize {
            Quantize::Bar => {
                let at_bar = timeline
                    .bar_index_at(timeline.bar_start_at_or_after(earliest))
                    .max(min_bar);
                (timeline.bar_start(at_bar), at_bar)
            }
            Quantize::Loop => {
                // The next end of a pass of *this* section, counted in bars so an odd time
                // signature cannot round the boundary away from the section's own grid. At least
                // pass 1: a section cannot end before its first pass is through.
                let bars = current.bars.max(1) as u64;
                let mut pass = (timeline
                    .bar_index_at(earliest)
                    .saturating_sub(current.start_bar)
                    / bars)
                    .max(1);
                loop {
                    let at_bar = current.start_bar + pass * bars;
                    let at = timeline.bar_start(at_bar);
                    if at >= earliest && at_bar >= min_bar {
                        return (at, at_bar);
                    }
                    pass += 1;
                }
            }
        }
    }

    /// Arm the section after the current one, or the one `to` names.
    fn arm_next(&mut self, est: u64, to: Option<usize>) -> String {
        let Some(current) = self.current else {
            return "No section is playing yet.".to_string();
        };
        let (at, at_bar, pushed) = self.change_position(est, current);
        let target = match to {
            Some(index) => Target::Section(index),
            None if current.index + 1 < self.plans.len() => Target::Section(current.index + 1),
            None => Target::End,
        };
        self.arm(target, at, at_bar);
        let (bars, beats) = self.sched.distance(est, at);
        // Where the switch lands, said in the section's own terms. The absolute bar index is no
        // use to a musician: it counts from whenever the engine was started, count-in included,
        // and a section that loops makes "bar 44 of a 41-bar arrangement" look like an error.
        let id = &self.plans[current.index].id;
        let len = current.bars.max(1) as u64;
        let rel = at_bar.saturating_sub(current.start_bar);
        let place = if rel % len == 0 {
            format!("at the end of pass {} of \"{id}\"", rel / len)
        } else {
            format!("at bar {} of \"{id}\" (pass {})", rel % len + 1, rel / len + 1)
        };
        let note = if pushed {
            " - after the take that is still recording; a take is never cut short"
        } else {
            ""
        };
        format!(
            "Switch to \"{}\" armed {place}, {}{note}.",
            self.target_label(target),
            lead_text(bars, beats)
        )
    }

    /// Schedule everything a target needs at `at` and remember it as armed.
    fn arm(&mut self, target: Target, at: u64, at_bar: u64) {
        let takes = match target {
            Target::Section(index) => self.schedule_section(index, at, at_bar),
            Target::End => {
                self.schedule_silence(at);
                Vec::new()
            }
        };
        self.armed = Some(Armed {
            target,
            at,
            at_bar,
            takes,
        });
    }

    /// The whole translation from target states to commands. Returns the tracks a take was
    /// scheduled on.
    fn schedule_section(&mut self, index: usize, start: u64, start_bar: u64) -> Vec<usize> {
        let plan = self.plans[index].clone();
        self.sched.bars = plan.bars;
        let end = self.sched.timeline.bar_start(start_bar + plan.bars as u64);
        let mut takes = Vec::new();

        for track in 0..self.tracks.len() {
            let want = plan.want[track];
            let name = self.tracks[track].name.clone();
            match want {
                ScoreState::Record => {
                    let scheduled = self.sched.record_at(track, &name, start, plan.bars);
                    self.push(scheduled);
                    let t = &mut self.tracks[track];
                    t.origin = start;
                    t.len = end - start;
                    t.len_bars = plan.bars;
                    t.layers = 1;
                    t.take_end = end;
                    t.take_end_bar = start_bar + plan.bars as u64;
                    // The take's own `StartPlay` at `end` leaves the track playing, which is what a
                    // section that loops needs and what the next section then finds.
                    t.sounding = true;
                    takes.push(track);
                }
                ScoreState::Overdub => {
                    // A further layer belongs on the **track's own** grid, `origin + n * loop_len`,
                    // not on the bar line the section starts on. The two are the same number to
                    // within a sample or two, and that difference is exactly what would break: a
                    // layer that starts a sample early is a sample short at the end of the pass,
                    // and a `StartOverdub` a sample before the running take's `StopRecord` is
                    // refused as "Busy". See `schedule::Scheduler::loop_grid_at_or_after`.
                    let t = &self.tracks[track];
                    let (take_start, take_end, defines_loop) = if t.len == 0 {
                        // An overdub on a track without a loop is simply the first take, over the
                        // section - the engine treats `StartOverdub` on an empty track that way.
                        (start, end, true)
                    } else {
                        // The grid point that *is* this section's start, even when it lies a few
                        // samples before the bar line: `origin + n * len` and the bar line are
                        // rounded separately and drift apart by up to a sample per pass. Asking
                        // strictly "at or after `start`" would then skip to the *next* grid point
                        // and start the layer a whole loop late - the first pass of the section
                        // would be lost and a release pushed a pass further (seen in Henry's
                        // score: "in 14 bars" in bar 3 of an eight-bar section). Half a beat of
                        // slack is far more than the drift and far less than any real loop length,
                        // which is always whole bars.
                        let slack = (self.sched.timeline.samples_per_beat() / 2.0) as u64;
                        let at = Scheduler::loop_grid_at_or_after(
                            t.origin,
                            t.len,
                            start.saturating_sub(slack),
                        );
                        // One whole pass, unless the section is genuinely shorter than the loop.
                        // The comparison is in **bars**, for the same rounding reason.
                        let stop = if plan.bars >= t.len_bars { at + t.len } else { end };
                        (at, stop, false)
                    };
                    let layers = self.tracks[track].layers;
                    let scheduled =
                        self.sched
                            .overdub_at(track, &name, take_start, take_end, layers);
                    self.push(scheduled);
                    let take_end_bar = self.sched.timeline.bar_index_at(take_end);
                    let t = &mut self.tracks[track];
                    if defines_loop {
                        t.origin = take_start;
                        t.len = take_end - take_start;
                        t.len_bars = plan.bars;
                        t.layers = 1;
                    } else {
                        t.layers = t.layers.saturating_add(1);
                    }
                    t.take_end = take_end;
                    t.take_end_bar = take_end_bar;
                    t.sounding = true;
                    takes.push(track);
                }
                ScoreState::Play => {
                    // The one case that must produce nothing: a track that is already sounding.
                    if !self.tracks[track].sounding && self.tracks[track].len > 0 {
                        let scheduled = self.sched.play_at(track, &name, start);
                        self.push(scheduled);
                        self.tracks[track].sounding = true;
                    }
                }
                ScoreState::Stop | ScoreState::HearThrough => {
                    if self.tracks[track].sounding {
                        let scheduled = self.sched.stop_play_at(track, &name, start);
                        self.push(scheduled);
                        self.tracks[track].sounding = false;
                    }
                }
            }
            self.tracks[track].target = want;
        }
        takes
    }

    /// Everything silent at `at` - the end of the score.
    fn schedule_silence(&mut self, at: u64) {
        for track in 0..self.tracks.len() {
            if self.tracks[track].sounding {
                let name = self.tracks[track].name.clone();
                let scheduled = self.sched.stop_play_at(track, &name, at);
                self.push(scheduled);
                self.tracks[track].sounding = false;
            }
            self.tracks[track].target = ScoreState::Stop;
        }
    }
}

/// `in 5 bars`, `in 1 bar and 2 beats`, `now`.
fn lead_text(bars: u32, beats: u32) -> String {
    let beats_text = |b: u32| if b == 1 { "1 beat".to_string() } else { format!("{b} beats") };
    match (bars, beats) {
        (0, 0) => "now".to_string(),
        (0, b) => format!("in {}", beats_text(b)),
        (1, 0) => "in 1 bar".to_string(),
        (t, 0) => format!("in {t} bars"),
        (1, b) => format!("in 1 bar and {}", beats_text(b)),
        (t, b) => format!("in {t} bars and {}", beats_text(b)),
    }
}

/// What a score says about how its tracks are set up, as the untimed commands that put an engine
/// into that state: pan, the two bus assignments, the latency compensation - and monitoring off,
/// which is the state the runner's picture starts from (`hear_through` and the takes switch it on
/// at the section boundary).
///
/// Untimed on purpose: these are setup, not musical events, and they are only sent while every
/// track is empty and no score is playing - on load, and again after "Clear all" has put the
/// tracks back to the values they were *started* with.
pub fn setup_commands(score: &CompiledScore) -> Vec<Command> {
    use super::bus::TrackSource;
    let mut out = Vec::with_capacity(score.tracks.len() * 5);
    for (track, spec) in score.tracks.iter().enumerate() {
        out.push(Command::SetPan {
            track,
            pan: spec.pan,
        });
        out.push(Command::SetTrackSend {
            track,
            source: TrackSource::Loop,
            send: spec.loop_send(),
        });
        out.push(Command::SetTrackSend {
            track,
            source: TrackSource::Monitor,
            send: spec.monitor_send(),
        });
        out.push(Command::SetTrackLatency {
            track,
            latency: spec.track_latency(),
        });
        // An instrument track keeps the monitoring it was started with: heard, unless the score
        // says monitor: false (the host starts the engine that way). The engine would ignore
        // the command anyway.
        if !spec.is_instrument() {
            out.push(Command::SetMonitor { track, on: false });
        }
    }
    out
}

/// Check a score against the tracks an engine is **already** running with.
///
/// The engine's track layout - how many tracks there are, what they are called and which inputs
/// they listen on - is decided when the engine starts and never changed underneath a running
/// session. Loading a score with a different layout is therefore refused rather than applied:
/// silently rewiring an input mid-song would send the next take to the wrong microphone, and a
/// track that disappears would take a recorded loop with it. The way out is the honest one - stop
/// the engine and start it with this score, which is what the `score` subcommand does anyway.
///
/// Pan and latency are deliberately **not** part of the comparison: both change where a track sits
/// or how far it is shifted, neither changes what a command means, and both are adjustable at
/// runtime by design.
pub fn check_tracks(score: &CompiledScore, running: &[TrackDef]) -> Result<(), String> {
    if score.tracks.len() != running.len() {
        return Err(format!(
            "The score has {} tracks, the running engine {}. Tracks cannot be rebuilt while the \
             engine runs - stop the engine, then start it with this score.",
            score.tracks.len(),
            running.len()
        ));
    }
    for (want, have) in score.tracks.iter().zip(running) {
        if want.name != have.name {
            return Err(format!(
                "Track {} is called \"{}\" in the score but \"{}\" in the running engine. \
                 Stop the engine, then start it with this score.",
                want.index + 1,
                want.name,
                have.name
            ));
        }
        if want.track_input() != have.input {
            return Err(format!(
                "Track \"{}\" listens to input {} in the score but to {} in the running engine. \
                 Changing an input while the engine runs would record the next take from the \
                 wrong source - stop the engine first.",
                want.name,
                want.track_input().label(),
                have.input.label()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
