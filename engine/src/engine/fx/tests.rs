//! Chain-level proofs. The individual effects are proven next to their own code; what is checked
//! here is the wiring: bypass, switching, presets, and that nothing in the chain can produce a
//! value that is not a number.

use super::*;

const RATE: u32 = 48_000;

fn sine(hz: f32, i: usize, amplitude: f32) -> f32 {
    amplitude * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin()
}

/// Peak of the chain's output for a steady sine, in dB relative to the input.
///
/// Half a second of settling first: every switch in the chain is a crossfade, and a filter needs
/// a few periods before its output is periodic.
fn response_db(chain: &mut Chain, hz: f32) -> f32 {
    let settle = RATE as usize / 2;
    for i in 0..settle {
        chain.process(sine(hz, i, 0.5));
    }
    let mut peak: f32 = 0.0;
    for i in settle..settle + RATE as usize / 4 {
        peak = peak.max(chain.process(sine(hz, i, 0.5)).abs());
    }
    20.0 * (peak / 0.5).log10()
}

/// A chain with only the passive parts of a preset switched on, so a frequency response really is
/// the filter's and not the compressor's.
fn filters_only(preset: FxPreset) -> Chain {
    let mut chain = Chain::new(RATE);
    chain.load_preset(preset);
    chain.set_enabled(FxSlot::Comp, false);
    chain.set_enabled(FxSlot::Delay, false);
    chain.set_enabled(FxSlot::Reverb, false);
    chain
}

// -------------------------------------------------------------------------------------------
// Bypass and the default state
// -------------------------------------------------------------------------------------------

/// The property every one of the 123 tests that existed before this module relies on: a track
/// that nobody has touched sounds exactly as it did before there were effects.
#[test]
fn a_fresh_chain_is_bit_identical_with_its_input() {
    let mut chain = Chain::new(RATE);
    assert!(chain.bypassed());
    assert_eq!(chain.preset(), FxPreset::Dry);
    for i in 0..10_000 {
        let x = sine(440.0, i, 0.9) + sine(37.0, i, 0.1);
        assert_eq!(chain.process(x), x, "Sample {i}");
    }
}

/// And it stays bit-identical after the chain has been loud: switching the bypass on crossfades
/// (so it does not click) and then becomes an exact pass-through (so it is a panic switch).
#[test]
fn bypass_becomes_bit_identical_once_the_crossfade_has_arrived() {
    let mut chain = Chain::new(RATE);
    chain.load_preset(FxPreset::Voice);
    for i in 0..RATE as usize {
        chain.process(sine(220.0, i, 0.6));
    }
    chain.set_bypass(true);
    // The crossfade has a 25 ms time constant, so it is inaudible after some 80 ms - but it only
    // *snaps* to an exact zero once it is under -100 dB, which takes about a dozen time constants.
    // One second is far more than enough and is what a panic switch gets in practice.
    for i in 0..RATE as usize {
        chain.process(sine(220.0, i, 0.6));
    }
    for i in 0..10_000 {
        let x = sine(311.0, i, 0.7);
        assert_eq!(chain.process(x), x, "Sample {i} nach dem Bypass");
    }
}

/// "Trocken" is the preset that switches everything off, so it has to be a pass-through too.
#[test]
fn the_dry_preset_switches_everything_off() {
    let mut chain = Chain::new(RATE);
    chain.load_preset(FxPreset::Voice);
    chain.load_preset(FxPreset::Dry);
    assert!(chain.bypassed());
    assert!(chain.settings().enabled.iter().all(|&on| !on));
    for i in 0..RATE as usize / 2 {
        chain.process(sine(220.0, i, 0.6));
    }
    for i in 0..5_000 {
        let x = sine(880.0, i, 0.5);
        assert_eq!(chain.process(x), x, "Sample {i}");
    }
}

/// A chain that is not bypassed but has nothing switched on is a wire as well - otherwise
/// switching the last effect off would leave a residue.
#[test]
fn an_empty_chain_that_is_not_bypassed_is_still_a_wire() {
    let mut chain = Chain::new(RATE);
    chain.set_bypass(false);
    for i in 0..2_000 {
        chain.process(sine(220.0, i, 0.5));
    }
    for i in 0..5_000 {
        let x = sine(1_000.0, i, 0.4);
        assert_eq!(chain.process(x), x, "Sample {i}");
    }
}

// -------------------------------------------------------------------------------------------
// Switching
// -------------------------------------------------------------------------------------------

#[test]
fn every_effect_can_be_switched_on_its_own() {
    let mut chain = Chain::new(RATE);
    chain.set_bypass(false);
    for slot in FxSlot::all() {
        assert!(!chain.enabled(slot), "{} startet aus", slot.label());
        chain.set_enabled(slot, true);
        assert!(chain.enabled(slot));
        for other in FxSlot::all() {
            if other != slot {
                assert!(!chain.enabled(other), "{} wurde mitgeschaltet", other.label());
            }
        }
        chain.set_enabled(slot, false);
    }
    assert_eq!(chain.preset(), FxPreset::Custom, "das war kein Preset mehr");
}

/// Switching an effect on while the music runs must not produce a step. The steepest a 220 Hz
/// sine of amplitude 0.5 can move between two samples is 0.0144; anything much above that is a
/// discontinuity, and a discontinuity is a click.
#[test]
fn switching_an_effect_on_does_not_step() {
    for slot in FxSlot::all() {
        let mut chain = Chain::new(RATE);
        chain.set_bypass(false);
        let mut i = 0usize;
        let mut previous = 0.0f32;
        for _ in 0..RATE as usize / 4 {
            previous = chain.process(sine(220.0, i, 0.5));
            i += 1;
        }
        chain.set_enabled(slot, true);
        let mut worst: f32 = 0.0;
        for _ in 0..RATE as usize / 2 {
            let y = chain.process(sine(220.0, i, 0.5));
            worst = worst.max((y - previous).abs());
            previous = y;
            i += 1;
        }
        assert!(
            worst < 0.05,
            "{}: Sprung von {worst} beim Einschalten",
            slot.label()
        );
    }
}

// -------------------------------------------------------------------------------------------
// The presets
// -------------------------------------------------------------------------------------------

/// What the voice preset promises, as numbers: rumble gone, presence up, air up, and the middle
/// of the voice left roughly where it was.
#[test]
fn the_voice_preset_removes_rumble_and_lifts_presence() {
    let mut chain = filters_only(FxPreset::Voice);
    let rumble = response_db(&mut chain, 40.0);
    assert!(rumble < -10.0, "40 Hz nur um {rumble} dB gesenkt");

    let mut chain = filters_only(FxPreset::Voice);
    let fundamental = response_db(&mut chain, 200.0);
    assert!(fundamental.abs() < 2.0, "200 Hz um {fundamental} dB verbogen");

    let mut chain = filters_only(FxPreset::Voice);
    let presence = response_db(&mut chain, 3_000.0);
    assert!(
        presence > 1.5 && presence < 3.5,
        "3 kHz um {presence} dB angehoben, erwartet rund 2.5 dB"
    );

    let mut chain = filters_only(FxPreset::Voice);
    let air = response_db(&mut chain, 14_000.0);
    assert!(air > 1.0, "Luft oben fehlt: {air} dB");
}

/// And the guitar preset: the quack between 2 and 4 kHz is down, the body boom is down, and the
/// high-pass sits higher than the voice's.
#[test]
fn the_guitar_preset_cuts_the_quack_and_the_boom() {
    let voice = FxPreset::Voice.settings();
    let guitar = FxPreset::PiezoGuitar.settings();
    assert!(
        guitar.high_pass_hz > voice.high_pass_hz,
        "Der Piezo-Hochpass muss hoeher liegen als der fuer die Stimme"
    );

    let mut chain = filters_only(FxPreset::PiezoGuitar);
    let quack = response_db(&mut chain, 3_000.0);
    assert!(quack < -3.0, "3 kHz nur um {quack} dB gesenkt");

    // The named range, 2 to 4 kHz, has to be inside the cut - not just its centre.
    for hz in [2_200.0, 4_000.0] {
        let mut chain = filters_only(FxPreset::PiezoGuitar);
        let db = response_db(&mut chain, hz);
        assert!(db < -1.5, "{hz} Hz nur um {db} dB gesenkt");
    }

    let mut chain = filters_only(FxPreset::PiezoGuitar);
    let boom = response_db(&mut chain, 180.0);
    assert!(boom < -2.0, "Koerperdroehnen bei 180 Hz: {boom} dB");

    // The sparkle above the cut comes back.
    let mut chain = filters_only(FxPreset::PiezoGuitar);
    let sparkle = response_db(&mut chain, 10_000.0);
    assert!(sparkle > 0.5, "Glanz oben fehlt: {sparkle} dB");
}

/// The compressor is the "Schmackes": with the voice preset a loud phrase and a quiet one end up
/// much closer together than they went in.
#[test]
fn the_voice_preset_evens_out_loud_and_quiet() {
    fn level_db(amplitude: f32) -> f32 {
        let mut chain = Chain::new(RATE);
        chain.load_preset(FxPreset::Voice);
        chain.set_enabled(FxSlot::Reverb, false);
        let mut peak: f32 = 0.0;
        for i in 0..RATE as usize {
            let y = chain.process(sine(220.0, i, amplitude));
            if i > RATE as usize / 2 {
                peak = peak.max(y.abs());
            }
        }
        20.0 * peak.log10()
    }
    // 18 dB apart going in.
    let loud = level_db(0.5);
    let quiet = level_db(0.5 / 8.0);
    let spread = loud - quiet;
    assert!(
        spread > 2.0 && spread < 14.0,
        "18 dB Eingangsunterschied wurden zu {spread} dB - erwartet deutlich weniger als 18"
    );
    // And the loud phrase does not lose much level on the way, which is what the makeup gain is
    // for: switching the preset on has to make the voice denser, not quieter. A sustained tone at
    // -6 dBFS is a harder case than a sung phrase (it never lets the compressor recover), and even
    // that comes out within a few dB of where it went in.
    let input_db = 20.0 * 0.5f32.log10();
    assert!(
        loud > input_db - 4.0,
        "Eingang {input_db:.1} dBFS, Ausgang {loud:.1} dBFS - das Preset macht deutlich leiser"
    );
}

#[test]
fn a_preset_keeps_its_name_until_a_knob_is_turned() {
    let mut chain = Chain::new(RATE);
    chain.load_preset(FxPreset::Voice);
    assert_eq!(chain.preset(), FxPreset::Voice);
    chain.load_preset(FxPreset::PiezoGuitar);
    assert_eq!(chain.preset(), FxPreset::PiezoGuitar);
    chain.set_param(FxParam::ReverbMix(0.4));
    assert_eq!(chain.preset(), FxPreset::Custom);
    assert_eq!(chain.settings().reverb_mix, 0.4);
    chain.load_preset(FxPreset::Voice);
    assert_eq!(chain.preset(), FxPreset::Voice);
    assert_eq!(chain.settings().reverb_mix, 0.18, "das Preset gewinnt wieder");
}

#[test]
fn preset_names_survive_the_round_trip_through_their_labels() {
    for preset in FxPreset::loadable() {
        assert_eq!(FxPreset::parse(preset.label()), Some(preset));
    }
    assert_eq!(FxPreset::parse("STIMME"), Some(FxPreset::Voice));
    assert_eq!(FxPreset::parse("bass"), None);
    assert_eq!(FxSlot::from_number(1), Some(FxSlot::HighPass));
    assert_eq!(FxSlot::from_number(5), Some(FxSlot::Reverb));
    assert_eq!(FxSlot::from_number(0), None);
    assert_eq!(FxSlot::from_number(6), None);
}

// -------------------------------------------------------------------------------------------
// Parameters
// -------------------------------------------------------------------------------------------

#[test]
fn every_parameter_lands_where_it_is_supposed_to_and_is_clamped() {
    let mut chain = Chain::new(RATE);
    chain.set_param(FxParam::HighPassHz(120.0));
    assert_eq!(chain.settings().high_pass_hz, 120.0);
    chain.set_param(FxParam::HighPassHz(5.0));
    assert_eq!(chain.settings().high_pass_hz, 20.0, "unten gedeckelt");

    chain.set_param(FxParam::BandHz { band: 1, hz: 2_500.0 });
    chain.set_param(FxParam::BandQ { band: 1, q: 2.0 });
    chain.set_param(FxParam::BandGainDb { band: 1, db: -6.0 });
    chain.set_param(FxParam::BandKind {
        band: 2,
        kind: BandKind::HighShelf,
    });
    let s = chain.settings();
    assert_eq!(s.bands[1].hz, 2_500.0);
    assert_eq!(s.bands[1].q, 2.0);
    assert_eq!(s.bands[1].gain_db, -6.0);
    assert_eq!(s.bands[2].kind, BandKind::HighShelf);
    // A band index that does not exist is ignored rather than panicking in the audio callback.
    chain.set_param(FxParam::BandGainDb { band: 9, db: 12.0 });
    assert_eq!(chain.settings().bands, s.bands);

    chain.set_param(FxParam::CompThresholdDb(-30.0));
    chain.set_param(FxParam::CompRatio(6.0));
    chain.set_param(FxParam::CompAttackMs(3.0));
    chain.set_param(FxParam::CompReleaseMs(400.0));
    chain.set_param(FxParam::CompKneeDb(4.0));
    chain.set_param(FxParam::CompMakeupDb(2.0));
    let c = chain.settings().comp;
    assert_eq!(
        (c.threshold_db, c.ratio, c.attack_ms, c.release_ms, c.knee_db, c.makeup_db),
        (-30.0, 6.0, 3.0, 400.0, 4.0, 2.0)
    );

    chain.set_param(FxParam::DelayNote(DelayNote::Quarter));
    chain.set_param(FxParam::DelayFeedback(2.0));
    chain.set_param(FxParam::DelayMix(0.4));
    assert_eq!(chain.settings().delay_note, DelayNote::Quarter);
    assert_eq!(chain.settings().delay_feedback, 0.95, "Feedback gedeckelt");
    assert_eq!(chain.settings().delay_mix, 0.4);

    chain.set_param(FxParam::ReverbSize(1.5));
    chain.set_param(FxParam::ReverbDamping(0.25));
    chain.set_param(FxParam::ReverbMix(0.3));
    assert_eq!(chain.settings().reverb_size, 1.0, "Groesse gedeckelt");
    assert_eq!(chain.settings().reverb_damping, 0.25);
    assert_eq!(chain.settings().reverb_mix, 0.3);
}

/// The tempo-synchronous delay, seen from the chain: the number the status reports has to be the
/// one the timeline dictates, and it has to follow a tempo change.
#[test]
fn the_delay_follows_the_tempo_through_the_chain() {
    let mut chain = Chain::new(RATE);
    chain.set_bypass(false);
    chain.set_enabled(FxSlot::Delay, true);
    chain.set_param(FxParam::DelayNote(DelayNote::Quarter));
    chain.set_quarter_samples(RATE as f64 * 60.0 / 120.0);
    assert_eq!(chain.status().delay_samples, 24_000);

    chain.set_quarter_samples(RATE as f64 * 60.0 / 90.0);
    for i in 0..RATE as usize / 4 {
        chain.process(sine(220.0, i, 0.3));
    }
    assert_eq!(chain.status().delay_samples, 32_000);
}

// -------------------------------------------------------------------------------------------
// Robustness
// -------------------------------------------------------------------------------------------

/// Everything on, everything at its limit, a signal that clips: nothing may become NaN or
/// infinite. A single NaN in a feedback loop poisons the whole track for the rest of the session.
#[test]
fn nothing_in_the_chain_can_produce_a_value_that_is_not_a_number() {
    let mut chain = Chain::new(RATE);
    chain.load_preset(FxPreset::Voice);
    for slot in FxSlot::all() {
        chain.set_enabled(slot, true);
    }
    chain.set_param(FxParam::DelayFeedback(0.95));
    chain.set_param(FxParam::DelayMix(1.0));
    chain.set_param(FxParam::ReverbSize(1.0));
    chain.set_param(FxParam::ReverbDamping(0.0));
    chain.set_param(FxParam::ReverbMix(1.0));
    chain.set_param(FxParam::CompMakeupDb(24.0));
    chain.set_param(FxParam::BandGainDb { band: 0, db: 24.0 });
    chain.set_param(FxParam::BandGainDb { band: 1, db: 24.0 });
    chain.set_param(FxParam::BandGainDb { band: 2, db: 24.0 });
    chain.set_quarter_samples(RATE as f64 * 60.0 / 100.0);

    for i in 0..RATE as usize * 4 {
        // Full scale, square-ish, with a DC offset: everything a converter could ever hand over.
        let x = if (i / 37) % 2 == 0 { 1.0 } else { -1.0 } * 0.999 + 0.001;
        let y = chain.process(x);
        assert!(y.is_finite(), "Sample {i} ist {y}");
    }
    // And then silence, where a reverb is at its most dangerous. Long enough for the chain's own
    // idea of its tail to run out - at these settings that is close to a minute of audio.
    for i in 0..(chain.tail_samples() as usize + 1_000) {
        let y = chain.process(0.0);
        assert!(y.is_finite(), "Stille-Sample {i} ist {y}");
    }
    assert_eq!(chain.process(0.0), 0.0, "die Kette muss ausklingen");
}

/// The idle detection: after the tail has run out a silent chain costs nothing and outputs
/// nothing, and the very next sample of music wakes it up again.
#[test]
fn a_silent_chain_goes_idle_and_wakes_up_again() {
    let mut chain = Chain::new(RATE);
    chain.load_preset(FxPreset::Voice);
    chain.set_quarter_samples(RATE as f64 * 60.0 / 100.0);
    assert!(chain.tail_samples() > RATE / 2, "der Schwanz ist zu kurz angesetzt");

    for i in 0..RATE as usize {
        chain.process(sine(220.0, i, 0.5));
    }
    for _ in 0..(chain.tail_samples() as usize + 1_000) {
        chain.process(0.0);
    }
    assert_eq!(chain.process(0.0), 0.0);
    // A single loud sample has to bring the chain back.
    let woken = chain.process(0.5);
    assert!(woken.abs() > 0.0, "die Kette bleibt stumm");
}

#[test]
fn the_status_letters_show_what_is_on() {
    let mut chain = Chain::new(RATE);
    chain.load_preset(FxPreset::Voice);
    let status = chain.status();
    assert_eq!(status.letters(), "HEK-R", "Delay ist im Stimme-Preset aus");
    assert!(status.active());
    assert_eq!(status.preset, FxPreset::Voice);

    chain.load_preset(FxPreset::Dry);
    let status = chain.status();
    assert_eq!(status.letters(), "-----");
    assert!(!status.active());
    assert_eq!(FxStatus::default().letters(), "-----");
}
