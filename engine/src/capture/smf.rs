//! A Standard MIDI File of what one track sent its instrument during a recording.
//!
//! Format 0, one track: the track name, the tempo and the time signature at tick 0, then every
//! message at its position. Positions are the recording's frames converted at the run's tempo
//! (which cannot change while anything is recorded), so a DAW that opens the file at the tempo it
//! says puts every note under the same sample of the stem it belongs to.

/// Ticks per quarter note in the file. 960 is what most DAWs write; at 120 BPM a tick is about half
/// a millisecond.
pub const DIVISION: u16 = 960;

/// The file for `events` (recording frame, message), in the order they were sent.
pub fn write(
    events: &[(u64, [u8; 3])],
    sample_rate: u32,
    bpm: f64,
    beats_per_bar: u32,
    beat_unit: u32,
    name: &str,
) -> Vec<u8> {
    let bpm = if bpm.is_finite() && bpm > 0.0 { bpm } else { 120.0 };
    let frames_per_tick = f64::from(sample_rate.max(1)) * 60.0 / bpm / f64::from(DIVISION);
    let mut track: Vec<u8> = Vec::new();
    // Track name.
    let name = name.as_bytes();
    track.extend_from_slice(&[0x00, 0xFF, 0x03]);
    var_len(&mut track, name.len() as u32);
    track.extend_from_slice(name);
    // Tempo: microseconds per quarter note.
    let us = (60_000_000.0 / bpm).round().clamp(1.0, 16_777_215.0) as u32;
    track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03, (us >> 16) as u8, (us >> 8) as u8, us as u8]);
    // Time signature: numerator, denominator as a power of two, clocks per click, 32nds per quarter.
    let denominator = beat_unit.max(1).trailing_zeros().min(7) as u8;
    let clocks = (96 / beat_unit.max(1)).clamp(1, 255) as u8;
    track.extend_from_slice(&[
        0x00,
        0xFF,
        0x58,
        0x04,
        beats_per_bar.clamp(1, 255) as u8,
        denominator,
        clocks,
        8,
    ]);
    let mut sorted: Vec<(u64, [u8; 3])> = events.to_vec();
    // Stable: messages at the same frame keep the order they were sent in.
    sorted.sort_by_key(|(frame, _)| *frame);
    let mut last = 0u64;
    for (frame, msg) in sorted {
        let tick = (frame as f64 / frames_per_tick).round() as u64;
        let delta = tick.saturating_sub(last);
        last = last.max(tick);
        var_len(&mut track, delta.min(0x0FFF_FFFF) as u32);
        let len = match msg[0] & 0xF0 {
            0xC0 | 0xD0 => 2,
            _ => 3,
        };
        track.extend_from_slice(&msg[..len]);
    }
    track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

    let mut out = Vec::with_capacity(track.len() + 22);
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&DIVISION.to_be_bytes());
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(track.len() as u32).to_be_bytes());
    out.extend_from_slice(&track);
    out
}

/// A MIDI variable-length quantity.
fn var_len(out: &mut Vec<u8>, mut value: u32) {
    let mut bytes = [0u8; 4];
    let mut n = 0;
    loop {
        bytes[n] = (value & 0x7F) as u8;
        n += 1;
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        out.push(if i > 0 { bytes[i] | 0x80 } else { bytes[i] });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_lengths_are_written_as_the_standard_says() {
        let mut v = Vec::new();
        for (value, bytes) in [
            (0u32, vec![0x00]),
            (0x7F, vec![0x7F]),
            (0x80, vec![0x81, 0x00]),
            (0x3FFF, vec![0xFF, 0x7F]),
            (0x0010_0000, vec![0xC0, 0x80, 0x00]),
        ] {
            v.clear();
            var_len(&mut v, value);
            assert_eq!(v, bytes, "{value:#x}");
        }
    }

    /// 120 BPM at 48 kHz: a quarter is 24000 frames, 960 ticks. A note on beat 2 lasting an eighth.
    #[test]
    fn a_note_lands_on_its_beat_in_ticks() {
        let data = write(
            &[(24_000, [0x90, 60, 100]), (36_000, [0x80, 60, 0])],
            48_000,
            120.0,
            4,
            4,
            "keys",
        );
        assert_eq!(&data[..4], b"MThd");
        assert_eq!(u16::from_be_bytes([data[12], data[13]]), DIVISION);
        let track = &data[22..];
        // name (3 + 1 + 4) + tempo 7 + time signature 8
        let events = &track[8 + 7 + 8..];
        assert_eq!(&events[..5], &[0x87, 0x40, 0x90, 60, 100], "960 Ticks bis zur Note");
        assert_eq!(&events[5..10], &[0x83, 0x60, 0x80, 60, 0], "480 Ticks lang");
        assert_eq!(&events[10..], &[0x00, 0xFF, 0x2F, 0x00]);
        // Tempo 500000 us per quarter.
        assert_eq!(&track[8..15], &[0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);
    }
}
