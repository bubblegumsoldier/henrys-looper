//! From a user action to timed engine commands - the part that needs no audio device.
//!
//! This is the same arithmetic the CLI's `live` subcommand does before it puts a command into the
//! queue, and it is kept separate from the audio plumbing for one reason: every rule in here
//! (quantise to the next bar, give the audio thread a head start, an overdub is exactly one pass
//! of the existing loop) can then be proven in a unit test instead of at the instrument.
//!
//! Two ideas carry the whole file:
//!
//! * **Everything musical happens on a bar boundary.** A command is stamped with an absolute
//!   sample position and the audio thread executes it exactly there, so it does not matter how
//!   long the command takes to get there - as long as it arrives before its own time.
//! * **`guard` is what makes that true.** Every scheduled position is at least `guard` samples
//!   ahead of where the engine is estimated to be, which is comfortably more than one round
//!   through the callbacks.

use std::time::Duration;

use looper_engine::engine::command::{Command, MAX_TRACKS};
use looper_engine::engine::live::TrackDef;
use looper_engine::engine::process::check_loop;
use looper_engine::engine::timeline::{TimeSignature, Timeline};
use looper_engine::engine::track::TrackState;

use crate::proto::TrackConfig;

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
}

impl Scheduler {
    /// `guard` is four buffers plus 20 ms, the same value the CLI uses: more than one round
    /// through both callbacks, so a command is always in the waiting room before it is due.
    pub fn new(timeline: Timeline, bars: u32, buffer_frames: u32) -> Self {
        Self {
            timeline,
            bars,
            guard: (buffer_frames * 4 + timeline.sample_rate() / 50) as u64,
        }
    }

    /// Where the audio thread is now, as well as the control side can know: the position of the
    /// newest status snapshot plus the time that has passed since it arrived.
    ///
    /// Both streams run on the interface clock, so this is good to a few samples - and the rest is
    /// absorbed by `guard` and by quantising to a bar boundary.
    pub fn estimated_pos(&self, last: Option<(u64, Duration)>) -> u64 {
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

    /// A new loop: replaces whatever the track holds, over the configured number of bars.
    pub fn record(&self, track: usize, name: &str, est: u64) -> Scheduled {
        let start = self.next_bar(est);
        let bar = self.timeline.bar_index_at(start);
        let end = self.timeline.position_of(bar + self.bars as u64, 0);
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

    /// A further layer. It covers exactly one pass of the existing loop; on an empty track it is
    /// the first take and gets the configured number of bars, so an overdub can never fail just
    /// because nothing is there yet.
    pub fn overdub(&self, track: usize, name: &str, est: u64, loop_len: u64, layers: u8) -> Scheduled {
        let start = self.next_bar(est);
        let bar = self.timeline.bar_index_at(start);
        let end = if loop_len > 0 {
            start + loop_len
        } else {
            self.timeline.position_of(bar + self.bars as u64, 0)
        };
        Scheduled {
            commands: vec![
                Command::StartOverdub { track, at: start },
                Command::StopRecord { track, at: end },
                Command::StartPlay { track, at: end },
            ],
            message: format!(
                "\"{name}\": Ebene {} ab Takt {}.",
                layers as usize + 1,
                bar + 1
            ),
        }
    }

    /// One button for three situations, exactly as on the keyboard: a scheduled recording is
    /// cancelled, a running one is closed on the next bar, and otherwise playback stops.
    pub fn stop(&self, track: usize, name: &str, est: u64, state: TrackState) -> Scheduled {
        match state {
            TrackState::Armed => Scheduled {
                commands: vec![Command::ClearTrack {
                    track,
                    at: self.soon(est),
                }],
                message: "Geplante Aufnahme abgebrochen.".to_string(),
            },
            TrackState::Recording | TrackState::Overdub => {
                let end = self.next_bar(est);
                Scheduled {
                    commands: vec![
                        Command::StopRecord { track, at: end },
                        Command::StartPlay { track, at: end },
                    ],
                    message: format!(
                        "Aufnahme endet mit Takt {}.",
                        self.timeline.bar_index_at(end)
                    ),
                }
            }
            _ => Scheduled {
                commands: vec![Command::StopPlay {
                    track,
                    at: self.soon(est),
                }],
                message: format!("\"{name}\": Wiedergabe gestoppt."),
            },
        }
    }

    pub fn play(&self, track: usize, name: &str, est: u64) -> Scheduled {
        let at = self.next_bar(est);
        Scheduled {
            commands: vec![Command::StartPlay { track, at }],
            message: format!(
                "\"{name}\": Wiedergabe ab Takt {}.",
                self.timeline.bar_index_at(at) + 1
            ),
        }
    }

    pub fn clear_track(&self, track: usize, name: &str, est: u64) -> Scheduled {
        Scheduled {
            commands: vec![Command::ClearTrack {
                track,
                at: self.soon(est),
            }],
            message: format!("\"{name}\" geleert."),
        }
    }

    pub fn clear_all(&self, est: u64) -> Scheduled {
        Scheduled {
            commands: vec![Command::ClearAll { at: self.soon(est) }],
            message: "Alles geleert - Zustand wie frisch gestartet.".to_string(),
        }
    }
}

/// Build a timeline for a tempo and check that a loop of `bars` bars can exist at it.
///
/// Both German messages come from the engine: [`Timeline::validate`] for the tempo and
/// [`check_loop`] for the loop that would be shorter than the latency compensation.
pub fn build_timeline(
    sample_rate: u32,
    bpm: f64,
    beats_per_bar: u32,
    beat_unit: u32,
    bars: u32,
    latency_samples: u64,
) -> Result<Timeline, String> {
    let signature = TimeSignature::new(beats_per_bar, beat_unit);
    Timeline::validate(sample_rate, bpm, signature)?;
    let timeline = Timeline::new(sample_rate, bpm, signature);
    check_loop(&timeline, bars, latency_samples)?;
    Ok(timeline)
}

/// Turn the track list of a start configuration into engine track definitions.
///
/// The same four rules the CLI applies, in the wording a window needs: at least one track, an
/// input the device actually has, unique names, and the track ceiling of the status snapshot.
pub fn resolve_tracks(
    configs: &[TrackConfig],
    input_channels: usize,
) -> Result<Vec<TrackDef>, String> {
    if input_channels == 0 {
        return Err("Das Geraet meldet keinen einzigen Eingangskanal.".to_string());
    }
    if configs.is_empty() {
        return Err("Kein Track angelegt. Es braucht mindestens einen.".to_string());
    }
    if configs.len() > MAX_TRACKS {
        return Err(format!(
            "{} Tracks angefragt, moeglich sind hoechstens {MAX_TRACKS}.",
            configs.len()
        ));
    }
    let mut defs = Vec::with_capacity(configs.len());
    for config in configs {
        let name = config.name.trim();
        if name.is_empty() {
            return Err("Ein Track ohne Namen geht nicht.".to_string());
        }
        if config.input_channel == 0 {
            return Err(format!(
                "Track \"{name}\": Eingaenge werden ab 1 gezaehlt, wie am Geraet beschriftet."
            ));
        }
        if config.input_channel as usize > input_channels {
            return Err(format!(
                "Track \"{name}\" soll auf Eingang {} hoeren, das Geraet liefert aber nur {input_channels} \
                 Eingangskanaele (also Eingang 1 bis {input_channels}).",
                config.input_channel
            ));
        }
        if defs.iter().any(|d: &TrackDef| d.name == name) {
            return Err(format!(
                "Der Trackname \"{name}\" kommt zweimal vor. Namen muessen eindeutig sein."
            ));
        }
        defs.push(TrackDef {
            name: name.to_string(),
            channel: config.input_channel as usize - 1,
        });
    }
    Ok(defs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn scheduler(bpm: f64, bars: u32) -> Scheduler {
        Scheduler::new(
            Timeline::new(RATE, bpm, TimeSignature::new(4, 4)),
            bars,
            128,
        )
    }

    fn config(name: &str, channel: u32) -> TrackConfig {
        TrackConfig {
            name: name.to_string(),
            input_channel: channel,
        }
    }

    #[test]
    fn the_head_start_is_four_buffers_plus_twenty_milliseconds() {
        assert_eq!(scheduler(120.0, 8).guard, 128 * 4 + 48_000 / 50);
    }

    /// Everything musical must land on a bar boundary that is still in the future by at least the
    /// head start - that is the whole point of the quantisation.
    #[test]
    fn a_recording_starts_on_the_next_reachable_bar_and_lasts_the_configured_bars() {
        let s = scheduler(120.0, 8);
        // 120 BPM, 4/4, 48 kHz: 96000 samples per bar, so bar boundaries are multiples of that.
        let plan = s.record(0, "gitarre", 100_000);
        assert_eq!(
            plan.commands,
            vec![
                Command::StartRecord {
                    track: 0,
                    at: 192_000
                },
                Command::StopRecord {
                    track: 0,
                    at: 192_000 + 8 * 96_000
                },
                Command::StartPlay {
                    track: 0,
                    at: 192_000 + 8 * 96_000
                },
            ]
        );
        assert!(plan.message.contains("Takt 3"), "{}", plan.message);
        assert!(plan.message.contains("8 Takte"), "{}", plan.message);

        // Exactly on a boundary the *next* one is taken, because the head start has to fit.
        let plan = s.record(1, "stimme", 96_000);
        assert_eq!(plan.commands[0], Command::StartRecord { track: 1, at: 192_000 });
    }

    /// An overdub is one pass of the existing loop, never the configured bar count - otherwise a
    /// layer would be longer than what it is layered onto.
    #[test]
    fn an_overdub_covers_exactly_one_pass_of_the_existing_loop() {
        let s = scheduler(120.0, 8);
        let loop_len = 4 * 96_000;
        let plan = s.overdub(0, "gitarre", 100_000, loop_len, 2);
        assert_eq!(
            plan.commands,
            vec![
                Command::StartOverdub {
                    track: 0,
                    at: 192_000
                },
                Command::StopRecord {
                    track: 0,
                    at: 192_000 + loop_len
                },
                Command::StartPlay {
                    track: 0,
                    at: 192_000 + loop_len
                },
            ]
        );
        assert!(plan.message.contains("Ebene 3"), "{}", plan.message);

        // On an empty track it behaves like a first take of the configured length.
        let plan = s.overdub(0, "gitarre", 100_000, 0, 0);
        assert_eq!(
            plan.commands[1],
            Command::StopRecord {
                track: 0,
                at: 192_000 + 8 * 96_000
            }
        );
    }

    #[test]
    fn stopping_means_three_different_things_depending_on_the_state() {
        let s = scheduler(120.0, 8);
        let armed = s.stop(0, "gitarre", 100_000, TrackState::Armed);
        assert!(matches!(armed.commands[0], Command::ClearTrack { .. }));
        assert!(armed.message.contains("abgebrochen"), "{}", armed.message);

        let recording = s.stop(0, "gitarre", 100_000, TrackState::Recording);
        assert_eq!(
            recording.commands,
            vec![
                Command::StopRecord {
                    track: 0,
                    at: 192_000
                },
                Command::StartPlay {
                    track: 0,
                    at: 192_000
                },
            ]
        );

        let playing = s.stop(0, "gitarre", 100_000, TrackState::Playing);
        assert!(matches!(playing.commands[0], Command::StopPlay { .. }));
        // Untimed-feeling actions still carry a timestamp, just not a quantised one.
        assert_eq!(playing.commands[0].at(), Some(100_000 + s.guard));
    }

    #[test]
    fn the_position_estimate_follows_the_interface_clock() {
        let s = scheduler(120.0, 8);
        assert_eq!(s.estimated_pos(None), 0);
        assert_eq!(
            s.estimated_pos(Some((48_000, Duration::from_millis(100)))),
            48_000 + 4_800
        );
    }

    /// A fractional beat length is the case that catches an incrementally advanced grid; the plan
    /// must still land on positions the timeline itself computes.
    #[test]
    fn odd_tempos_and_time_signatures_stay_on_the_timeline_grid() {
        let timeline = Timeline::new(RATE, 137.0, TimeSignature::new(7, 8));
        let s = Scheduler::new(timeline, 5, 128);
        let plan = s.record(0, "t", 1_000_000);
        let Command::StartRecord { at: start, .. } = plan.commands[0] else {
            panic!("erstes Kommando ist die Aufnahme");
        };
        let bar = timeline.bar_index_at(start);
        assert_eq!(start, timeline.bar_start(bar), "Start liegt auf einer Taktgrenze");
        assert_eq!(
            plan.commands[1].at(),
            Some(timeline.position_of(bar + 5, 0)),
            "Ende liegt fuenf Takte spaeter, absolut gerechnet"
        );
    }

    #[test]
    fn a_tempo_the_loop_cannot_carry_is_refused_in_german() {
        assert!(build_timeline(RATE, 100.0, 4, 4, 8, 827).is_ok());
        let err = build_timeline(RATE, 500.0, 4, 4, 8, 827).expect_err("BPM ausserhalb 1..400");
        assert!(err.contains("400"), "{err}");
        let err = build_timeline(RATE, 100.0, 4, 3, 8, 827).expect_err("Zaehlzeit 3 gibt es nicht");
        assert!(err.contains("Notenlaenge"), "{err}");
        // One bar at 400 BPM is 28800 samples, still longer than the compensation; a loop shorter
        // than the compensation is what check_loop exists for.
        let err = build_timeline(RATE, 400.0, 1, 4, 1, 20_000).expect_err("Loop zu kurz");
        assert!(err.contains("Latenzkompensation"), "{err}");
    }

    #[test]
    fn track_definitions_are_checked_against_the_device() {
        let ok = resolve_tracks(&[config("stimme", 1), config("gitarre", 2)], 2).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].channel, 0, "Kanal 1 am Geraet ist Index 0");
        assert_eq!(ok[1].channel, 1);

        for (configs, needle) in [
            (vec![config("stimme", 3)], "Eingang 3"),
            (vec![config("stimme", 0)], "ab 1"),
            (vec![config("  ", 1)], "ohne Namen"),
            (vec![config("a", 1), config("a", 2)], "eindeutig"),
            (vec![], "mindestens einen"),
        ] {
            let err = resolve_tracks(&configs, 2).expect_err(needle);
            assert!(err.contains(needle), "{err}");
        }
        assert!(resolve_tracks(&[config("a", 1)], 0).is_err(), "Geraet ohne Eingang");

        let many: Vec<TrackConfig> = (1..=MAX_TRACKS + 1)
            .map(|i| config(&format!("t{i}"), 1))
            .collect();
        assert!(resolve_tracks(&many, 8).is_err(), "Obergrenze greift");
    }
}
