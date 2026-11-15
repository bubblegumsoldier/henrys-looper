//! Turning a start configuration into something the engine can open.
//!
//! The arithmetic that used to live here - which grid a take snaps to, how much head start a
//! command gets, how long an overdub runs - moved into the engine library as
//! [`looper_engine::engine::schedule`], because the CLI does exactly the same thing and having it
//! twice meant every rule had to be changed twice while only one copy was under test. The two
//! functions that stayed are the ones that need the app's own wire format: a start configuration
//! carries [`TrackConfig`]s, which the engine knows nothing about.

use looper_engine::engine::command::MAX_TRACKS;
use looper_engine::engine::frame::TrackInput;
use looper_engine::engine::live::{MAX_LATENCY_FRAMES, TrackDef};
use looper_engine::engine::process::check_loop;
use looper_engine::engine::timeline::{TimeSignature, Timeline};

pub use looper_engine::engine::schedule::{Quantize, Scheduled, Scheduler};

use crate::proto::TrackConfig;

/// Build a timeline for a tempo and check that a loop of `bars` bars can exist at it.
///
/// Both messages come from the engine: [`Timeline::validate`] for the tempo and
/// [`check_loop`] for the loop that would be shorter than the latency compensation.
/// `latency_frames` is the **largest** compensation any track uses, not the default: the loop has
/// to be longer than the write pointer trails the play pointer on the worst of them. Callers get
/// that number from `looper_engine::engine::process::max_latency`.
pub fn build_timeline(
    sample_rate: u32,
    bpm: f64,
    beats_per_bar: u32,
    beat_unit: u32,
    bars: u32,
    latency_frames: u64,
) -> Result<Timeline, String> {
    let signature = TimeSignature::new(beats_per_bar, beat_unit);
    Timeline::validate(sample_rate, bpm, signature)?;
    let timeline = Timeline::new(sample_rate, bpm, signature);
    check_loop(&timeline, bars, latency_frames)?;
    Ok(timeline)
}

/// Turn the track list of a start configuration into engine track definitions.
///
/// The same rules the CLI applies, in the wording a window needs: at least one track, inputs the
/// device actually has, two *different* inputs for a stereo track, a pan inside the range the
/// panner has, unique names, and the track ceiling of the status snapshot.
pub fn resolve_tracks(
    configs: &[TrackConfig],
    input_channels: usize,
) -> Result<Vec<TrackDef>, String> {
    if input_channels == 0 {
        return Err("The device reports no input channel at all.".to_string());
    }
    if configs.is_empty() {
        return Err("No track set up. At least one is needed.".to_string());
    }
    if configs.len() > MAX_TRACKS {
        return Err(format!(
            "{} tracks requested, at most {MAX_TRACKS} are possible.",
            configs.len()
        ));
    }
    let mut defs = Vec::with_capacity(configs.len());
    for config in configs {
        let name = config.name.trim();
        if name.is_empty() {
            return Err("A track needs a name.".to_string());
        }
        let channel = |number: u32| -> Result<usize, String> {
            if number == 0 {
                return Err(format!(
                    "Track \"{name}\": inputs are counted from 1, as labelled on the device."
                ));
            }
            if number as usize > input_channels {
                return Err(format!(
                    "Track \"{name}\" is set to listen to input {number}, but the device only has \
                     {input_channels} input channels (input 1 to {input_channels})."
                ));
            }
            Ok(number as usize - 1)
        };
        let left = channel(config.input_channel)?;
        let input = match config.input_channel_right {
            Some(right) => {
                let right = channel(right)?;
                if right == left {
                    return Err(format!(
                        "Track \"{name}\" is set up as stereo but names input {} twice. \
                         A stereo track needs two different inputs.",
                        left + 1
                    ));
                }
                TrackInput::Stereo { left, right }
            }
            None => TrackInput::Mono(left),
        };
        if !(-1.0..=1.0).contains(&config.pan) {
            return Err(format!(
                "Track \"{name}\": pan must be between -1 (hard left) and 1 (hard right), \
                 not {}.",
                config.pan
            ));
        }
        if defs.iter().any(|d: &TrackDef| d.name == name) {
            return Err(format!(
                "The track name \"{name}\" appears twice. Names must be unique."
            ));
        }
        if config.latency_trim.unsigned_abs() as i64 > MAX_LATENCY_FRAMES
            || config.latency_frames.map(i64::from).unwrap_or(0) > MAX_LATENCY_FRAMES
        {
            return Err(format!(
                "Track \"{name}\": the latency compensation must stay within {MAX_LATENCY_FRAMES} \
                 frames (two seconds at 48 kHz). That is not a latency, it is a typo."
            ));
        }
        defs.push(TrackDef {
            name: name.to_string(),
            input,
            pan: config.pan,
            latency: config.track_latency(),
            loop_send: config.loop_send(name)?,
            monitor_send: config.monitor_send(name)?,
        });
    }
    Ok(defs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use looper_engine::engine::command::Command;
    use looper_engine::engine::frame::Channels;

    const RATE: u32 = 48_000;

    fn config(name: &str, channel: u32) -> TrackConfig {
        TrackConfig::mono(name, channel)
    }

    fn stereo(name: &str, left: u32, right: u32) -> TrackConfig {
        TrackConfig {
            input_channel_right: Some(right),
            ..TrackConfig::mono(name, left)
        }
    }

    #[test]
    fn a_tempo_the_loop_cannot_carry_is_refused_in_words() {
        assert!(build_timeline(RATE, 100.0, 4, 4, 8, 827).is_ok());
        let err = build_timeline(RATE, 500.0, 4, 4, 8, 827).expect_err("BPM ausserhalb 1..400");
        assert!(err.contains("400"), "{err}");
        let err = build_timeline(RATE, 100.0, 4, 3, 8, 827).expect_err("Zaehlzeit 3 gibt es nicht");
        // Wording-robust: the engine's sentence names the value it refuses.
        assert!(err.contains(" 3 "), "{err}");
        // One bar at 400 BPM is 28800 samples, still longer than the compensation; a loop shorter
        // than the compensation is what check_loop exists for.
        let err = build_timeline(RATE, 400.0, 1, 4, 1, 20_000).expect_err("Loop zu kurz");
        assert!(err.contains("20000"), "{err}");
    }

    #[test]
    fn track_definitions_are_checked_against_the_device() {
        let ok = resolve_tracks(&[config("stimme", 1), config("gitarre", 2)], 2).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(
            ok[0].input,
            TrackInput::Mono(0),
            "Kanal 1 am Geraet ist Index 0"
        );
        assert_eq!(ok[1].input, TrackInput::Mono(1));
        assert_eq!(ok[0].pan, 0.0, "ohne Angabe sitzt ein Track in der Mitte");

        for (configs, needle) in [
            (vec![config("stimme", 3)], "input 3"),
            (vec![config("stimme", 0)], "from 1"),
            (vec![config("  ", 1)], "needs a name"),
            (vec![config("a", 1), config("a", 2)], "unique"),
            (vec![], "least one"),
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

    /// A second input channel makes a stereo track, and every way of getting the pair wrong has
    /// its own sentence before a single buffer is allocated.
    #[test]
    fn a_second_input_channel_makes_a_stereo_track() {
        let ok = resolve_tracks(&[config("stimme", 1), stereo("klavier", 3, 4)], 4).unwrap();
        assert_eq!(ok[0].input.channels(), Channels::Mono);
        assert_eq!(ok[1].input, TrackInput::Stereo { left: 2, right: 3 });
        assert_eq!(ok[1].input.channels(), Channels::Stereo);

        // The right-hand channel is checked against the device exactly like the left one.
        let err = resolve_tracks(&[stereo("klavier", 2, 3)], 2).expect_err("Kanal 3 fehlt");
        assert!(err.contains("input 3"), "{err}");

        let err = resolve_tracks(&[stereo("klavier", 2, 2)], 4).expect_err("zweimal derselbe");
        assert!(err.contains("two different"), "{err}");

        let far = TrackConfig {
            pan: 1.5,
            ..TrackConfig::mono("stimme", 1)
        };
        let err = resolve_tracks(&[far], 2).expect_err("Panorama zu weit");
        assert!(err.contains("pan must be"), "{err}");
    }

    /// The per-track compensation travels from the start configuration into the track definition
    /// unchanged, and an absurd number is caught before a device is opened.
    #[test]
    fn a_track_configuration_carries_its_latency_into_the_engine() {
        let plain = resolve_tracks(&[config("stimme", 1)], 2).unwrap();
        assert!(
            plain[0].latency.inherits(),
            "ohne Angabe folgt ein Track der globalen Vorgabe"
        );

        let cantabile = TrackConfig {
            latency_frames: Some(512),
            latency_trim: 96,
            ..TrackConfig::mono("cantabile", 1)
        };
        let resolved = resolve_tracks(&[cantabile], 2).unwrap();
        assert_eq!(resolved[0].latency.measured, Some(512));
        assert_eq!(resolved[0].latency.trim, 96);
        assert_eq!(resolved[0].latency.resolve(827), 608);

        for bad in [
            TrackConfig {
                latency_frames: Some(999_999),
                ..TrackConfig::mono("a", 1)
            },
            TrackConfig {
                latency_trim: -999_999,
                ..TrackConfig::mono("a", 1)
            },
        ] {
            let err = resolve_tracks(&[bad], 2).expect_err("absurde Latenz");
            assert!(err.contains("typo"), "{err}");
        }
    }

    /// The scheduler itself is proven in the engine library, where it now lives. This checks the
    /// one thing that is the app's business: that the re-export is the same type and that the app
    /// gets the loop grid by default, which is what the window's start configuration promises.
    #[test]
    fn the_re_exported_scheduler_defaults_to_the_loop_grid() {
        let timeline = Timeline::new(RATE, 120.0, TimeSignature::new(4, 4));
        let mut s = Scheduler::new(timeline, 8, 128, Quantize::default());
        assert_eq!(s.quantize, Quantize::Loop);
        assert_eq!(s.guard, 128 * 4 + RATE as u64 / 50);
        // 120 BPM, 4/4: 96000 samples per bar, so the eight-bar grid sits at 768000.
        let plan = s.record(0, "gitarre", 100_000);
        assert_eq!(
            plan.commands[0],
            Command::StartRecord {
                track: 0,
                at: 8 * 96_000
            }
        );
    }

    /// The bus assignment travels from the configuration into the track definition, and a word that
    /// is not a bus is refused by name rather than turned into silence.
    #[test]
    fn a_track_definition_carries_its_bus_assignment() {
        use looper_engine::engine::bus::BusSend;

        let plain = resolve_tracks(&[config("stimme", 1)], 2).expect("ein Track");
        assert_eq!(plain[0].loop_send, BusSend::BOTH);
        assert_eq!(plain[0].monitor_send, BusSend::MONITOR);

        let cue = TrackConfig {
            bus: Some("monitor".to_string()),
            monitor_bus: Some("none".to_string()),
            ..config("klick_gtr", 2)
        };
        let defs = resolve_tracks(&[cue], 2).expect("ein Track");
        assert_eq!(defs[0].loop_send, BusSend::MONITOR);
        assert_eq!(defs[0].monitor_send, BusSend::NONE);

        let wrong = TrackConfig {
            bus: Some("buehne".to_string()),
            ..config("stimme", 1)
        };
        let err = resolve_tracks(&[wrong], 2).expect_err("kein Bus");
        assert!(err.contains("buehne"), "{err}");
        assert!(err.contains("main+monitor"), "die Liste steht dabei: {err}");
    }
}
