//! Proofs for the `mixer:` section: what a written entry means, that the engine's state is
//! written back as an entry meaning exactly that state, and that the commands put a track there -
//! also onto an engine built anew with its tracks in a different order.

use super::*;
use crate::engine::fx::{Chain, CompSettings};
use crate::engine::sim::{Sim, SimSpec, TrackSpec, silence};
use crate::score::compile_score;

/// What the engine does with the commands of one chain, without an engine: the same calls on a
/// bare chain the audio thread would make.
fn apply_to_chain(chain: &mut Chain, commands: &[Command]) {
    for command in commands {
        match *command {
            Command::LoadFxPreset { preset, .. } => chain.load_preset(preset),
            Command::SetFxEnabled { slot, on, .. } => chain.set_enabled(slot, on),
            Command::SetFxParam { param, .. } => chain.set_param(param),
            Command::SetFxBypass { on, .. } => chain.set_bypass(on),
            _ => {}
        }
    }
}

fn chain_status(chain: &mut Chain) -> TrackStatus {
    TrackStatus {
        fx: chain.status(),
        ..TrackStatus::default()
    }
}

fn entry(yaml: &str) -> TrackMixSource {
    let text = format!(
        "bpm: 100\ntracks:\n  voice: {{input: 1}}\nsections:\n  - {{id: a, bars: 1}}\nmixer:\n  tracks:\n    voice: {yaml}\n"
    );
    let score = compile_score(&text).unwrap_or_else(|e| panic!("{e}"));
    score.mixer.expect("mixer").tracks.get("voice").cloned().expect("voice")
}

/// Some chains a musician plausibly ends up with.
fn chains() -> Vec<(bool, FxPreset, ChainSettings)> {
    let voice = FxPreset::Voice.settings();
    let mut tweaked = voice;
    tweaked.comp = CompSettings {
        threshold_db: -12.0,
        ..voice.comp
    };
    tweaked.bands[1].gain_db = 4.5;
    let mut dry_reverb = ChainSettings::default();
    dry_reverb.enabled[FxSlot::Reverb.index()] = true;
    dry_reverb.reverb_mix = 0.25;
    let mut guitar_delay = FxPreset::PiezoGuitar.settings();
    guitar_delay.enabled[FxSlot::Delay.index()] = true;
    guitar_delay.delay_note = DelayNote::Quarter;
    guitar_delay.bands[2].kind = BandKind::Peak;
    vec![
        (true, FxPreset::Dry, ChainSettings::default()),
        (false, FxPreset::Voice, voice),
        (true, FxPreset::Voice, voice),
        (false, FxPreset::Custom, tweaked),
        (false, FxPreset::Custom, dry_reverb),
        (true, FxPreset::Custom, dry_reverb),
        (false, FxPreset::Custom, guitar_delay),
    ]
}

#[test]
fn a_preset_alone_is_the_chain_the_button_loads() {
    let mix = entry("{preset: voice}").resolve();
    assert_eq!(mix.settings, FxPreset::Voice.settings());
    assert!(!mix.bypass);
    assert_eq!(mix.shown_preset(), FxPreset::Voice);
    assert_eq!(mix.gain, 1.0);
    assert!(!mix.mute);

    let dry = entry("{}").resolve();
    assert_eq!(dry.settings, ChainSettings::default());
    assert!(dry.bypass, "nothing switched on: out of the signal path, as 'dry' loads it");
}

#[test]
fn an_effect_line_overrides_only_what_it_names() {
    let mix = entry("{preset: voice, comp: {threshold: -12}, reverb: {on: false}}").resolve();
    let voice = FxPreset::Voice.settings();
    assert_eq!(mix.settings.comp.threshold_db, -12.0);
    assert_eq!(mix.settings.comp.ratio, voice.comp.ratio);
    assert!(!mix.settings.enabled[FxSlot::Reverb.index()]);
    assert_eq!(mix.settings.reverb_mix, voice.reverb_mix);
    assert_eq!(mix.shown_preset(), FxPreset::Custom);

    // A single effect on a dry chain is audible without a word about bypass.
    let reverb = entry("{reverb: {on: true, mix: 0.3}}").resolve();
    assert!(!reverb.bypass);
}

#[test]
fn levels_are_written_in_tenths_of_a_db_and_read_back() {
    assert_eq!(gain_to_db(1.0), 0.0);
    assert_eq!(gain_to_db(0.5), -6.0);
    assert_eq!(gain_to_db(0.0), DB_FLOOR);
    assert_eq!(gain_to_db(MAX_TRACK_GAIN), 12.0);
    assert_eq!(db_to_gain(DB_FLOOR, 4.0), 0.0);
    assert_eq!(db_to_gain(-150.0, 4.0), 0.0);
    assert_eq!(db_to_gain(f64::NEG_INFINITY, 4.0), 0.0);
    assert_eq!(db_to_gain(30.0, MAX_TRACK_GAIN), MAX_TRACK_GAIN);
    for db in [-60.0, -12.5, -3.0, 0.0, 0.1, 6.0, 11.9] {
        assert_eq!(gain_to_db(db_to_gain(db, 4.0)), db, "{db} dB");
    }
    assert_eq!(clean(0.62), 0.62);
}

#[test]
fn the_state_is_written_as_the_entry_that_means_it() {
    for (bypass, shown, settings) in chains() {
        let written = TrackMixSource::from_state(0.5, true, bypass, shown, &settings);
        let back = written.resolve();
        assert_eq!(back.settings, settings, "{written:?}");
        assert_eq!(back.bypass, bypass, "{written:?}");
        assert_eq!(back.gain, db_to_gain(-6.0, MAX_TRACK_GAIN));
        assert!(back.mute);
    }
    // The plain cases read as plainly as they are.
    let voice = TrackMixSource::from_state(1.0, false, false, FxPreset::Voice, &FxPreset::Voice.settings());
    assert_eq!(voice.preset.as_deref(), Some("voice"));
    assert!(!voice.has_effect_lines());
    assert_eq!(voice.bypass, None);
    assert_eq!(voice.mute, None, "an unmuted track does not say so");
    let dry = TrackMixSource::from_state(1.0, false, true, FxPreset::Dry, &ChainSettings::default());
    assert_eq!(dry, TrackMixSource { gain: Some(0.0), ..Default::default() });
    // One knob off the voice preset is one line, not a whole chain.
    let (_, _, tweaked) = chains()[3];
    let entry = TrackMixSource::from_state(1.0, false, false, FxPreset::Custom, &tweaked);
    assert_eq!(entry.preset.as_deref(), Some("voice"));
    assert_eq!(entry.comp, Some(CompLine { threshold: Some(-12.0), ..Default::default() }));
    assert_eq!(entry.band2, Some(BandLine { gain: Some(4.5), ..Default::default() }));
    assert!(entry.reverb.is_none() && entry.delay.is_none());
}

#[test]
fn the_commands_put_a_chain_where_the_entry_says() {
    for (bypass, shown, settings) in chains() {
        let target = TrackMixSource::from_state(1.0, false, bypass, shown, &settings).resolve();
        // From a fresh chain, and from one that is somewhere else entirely.
        for start in [FxPreset::Dry, FxPreset::PiezoGuitar] {
            let mut chain = Chain::new(48_000);
            chain.load_preset(start);
            let now = chain_status(&mut chain);
            apply_to_chain(&mut chain, &track_commands(0, &target, &now));
            assert_eq!(chain.settings(), target.settings);
            assert_eq!(chain.bypassed(), target.bypass);
            assert_eq!(chain.preset(), target.shown_preset(), "from {start:?}");
        }
    }
}

#[test]
fn a_chain_that_already_is_the_target_is_left_alone() {
    let target = entry("{gain: -3, preset: voice}").resolve();
    let mut chain = Chain::new(48_000);
    chain.load_preset(FxPreset::Voice);
    let commands = track_commands(2, &target, &chain_status(&mut chain));
    assert_eq!(
        commands,
        vec![
            Command::SetTrackGain { track: 2, gain: db_to_gain(-3.0, MAX_TRACK_GAIN) },
            Command::SetTrackMute { track: 2, on: false },
        ]
    );
}

const SCORE: &str = "\
bpm: 100
tracks:
  voice: {input: 1}
  guitar: {input: 2}
sections:
  - {id: a, bars: 1}
mixer:
  buses: {main: -2, monitor: -6, click: -12, inputs: -1.5}
  tracks:
    voice: {gain: -3.5, preset: voice}
    guitar:
      gain: 2
      mute: true
      preset: piezo_guitar
      comp: {threshold: -10}
";

fn sim(tracks: usize) -> Sim {
    Sim::new(
        SimSpec {
            tracks: (0..tracks).map(TrackSpec::on).collect(),
            input_channels: tracks,
            loopback_channel: None,
            ..SimSpec::default()
        },
        silence(),
    )
}

/// Apply a section to a simulated engine whose tracks are called `names`, as the app does after
/// starting or rebuilding one.
fn apply(sim: &mut Sim, mixer: &MixerSource, names: &[&str]) -> Status {
    sim.run_blocks(2);
    let status = sim.latest_status().expect("status");
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    for command in mixer_commands(mixer, &names, status.tracks()) {
        sim.send(command);
    }
    sim.run_blocks(4);
    sim.latest_status().expect("status")
}

#[test]
fn a_rebuilt_engine_gets_every_track_its_mix_by_name() {
    let mixer = compile_score(SCORE).expect("compiles").mixer.expect("mixer");
    let voice = mixer.tracks.get("voice").unwrap().resolve();
    let guitar = mixer.tracks.get("guitar").unwrap().resolve();

    // The engine the score was written on, and one rebuilt with the tracks the other way round
    // plus one the section does not mention.
    for names in [vec!["voice", "guitar"], vec!["guitar", "extra", "voice"]] {
        let mut engine = sim(names.len());
        let status = apply(&mut engine, &mixer, &names);
        let at = |name: &str| names.iter().position(|n| *n == name).unwrap();
        let v = status.tracks()[at("voice")];
        let g = status.tracks()[at("guitar")];
        assert_eq!(v.gain, voice.gain);
        assert!(!v.muted);
        assert_eq!(v.fx.preset, FxPreset::Voice);
        assert_eq!(v.fx.settings, voice.settings);
        assert_eq!(g.gain, guitar.gain);
        assert!(g.muted);
        assert_eq!(g.fx.preset, FxPreset::Custom);
        assert_eq!(g.fx.settings.comp.threshold_db, -10.0);
        assert_eq!(g.fx.settings, guitar.settings);
        if let Some(extra) = names.iter().position(|n| *n == "extra") {
            let e = status.tracks()[extra];
            assert_eq!((e.gain, e.muted, e.fx.preset), (1.0, false, FxPreset::Dry), "untouched");
        }
        assert_eq!(status.bus_gain(Bus::Main), db_to_gain(-2.0, MAX_BUS_GAIN));
        assert_eq!(status.bus_gain(Bus::Monitor), db_to_gain(-6.0, MAX_BUS_GAIN));
        assert_eq!(status.click_gain, db_to_gain(-12.0, MAX_LEVEL_GAIN));
        assert_eq!(status.monitor_gain, db_to_gain(-1.5, MAX_LEVEL_GAIN));

        // And what the engine then reports is written back as the same section.
        for (name, written) in mixer.tracks.iter() {
            let back = TrackMixSource::from_status(&status.tracks()[at(name)]);
            assert_eq!(back.resolve(), written.resolve(), "{name}");
        }
        assert_eq!(&BusLevels::from_status(&status), mixer.buses.as_ref().unwrap());
    }
}
