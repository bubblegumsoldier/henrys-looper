//! A pretend controller: a virtual MIDI source that shows up in every program's port list.
//!
//! Made for testing the learn mode, the auto-connect and the hot-plug handling without a device on
//! the desk. Only on macOS and Linux - Windows has no virtual ports without a third-party driver.
//!
//! ```text
//! cargo run -p looper-engine --example midi-virtual -- "Testpad"
//! ```
//!
//! The port exists while the program runs. Commands come on standard input, one per line:
//!
//! ```text
//! note 36 [vel] [ms]      note on (channel 1, velocity 100), note off after ms (default 30)
//! on 36 [vel]             note on only
//! off 36                  note off only
//! chord 60 64 67 [ms]     the notes together, held for ms (default 500), velocity 100
//! bend 4096               pitch bend, -8192..8191 (0 is the centre)
//! grid 120 8 [note]       n notes on the beat at that tempo (default note 60, 50 ms long) -
//!                         for timing measurements; prints when each one was sent
//! cc 3 64                 controller 3 to 64
//! ch 2                    following commands on channel 2
//! sleep 500               wait (milliseconds)
//! quit                    close the port and exit (like pulling the cable)
//! ```
//!
//! For an instrument track: `chord 60 64 67 800`, `grid 100 16`. Keep the volume low.

#[cfg(unix)]
fn main() {
    use std::io::BufRead;
    use std::time::Duration;

    use midir::MidiOutput;
    use midir::os::unix::VirtualOutput;

    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Looper Testpad".to_string());
    let output = MidiOutput::new("Looper Testcontroller").expect("MIDI system");
    let mut port = output
        .create_virtual(&name)
        .unwrap_or_else(|e| panic!("virtual port \"{name}\": {e}"));
    println!("ready: {name}");

    let mut channel: u8 = 0;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize, default: u8| -> u8 {
            words.get(i).and_then(|w| w.parse().ok()).unwrap_or(default)
        };
        let ms_at = |i: usize, default: u64| -> u64 {
            words.get(i).and_then(|w| w.parse().ok()).unwrap_or(default)
        };
        let sent = match words.first().copied() {
            Some("note") => {
                let n = num(1, 36);
                let a = port.send(&[0x90 | channel, n, num(2, 100)]);
                std::thread::sleep(Duration::from_millis(ms_at(3, 30)));
                a.and(port.send(&[0x80 | channel, n, 0]))
            }
            Some("chord") => {
                // The last number is the duration if there are at least two notes before it.
                let numbers: Vec<u64> = words[1..].iter().filter_map(|w| w.parse().ok()).collect();
                let (notes, ms) = match numbers.len() {
                    0 => (vec![60, 64, 67], 500),
                    1 | 2 => (numbers.clone(), 500),
                    _ => (numbers[..numbers.len() - 1].to_vec(), numbers[numbers.len() - 1]),
                };
                let notes: Vec<u8> = notes.iter().map(|&n| n.min(127) as u8).collect();
                let mut result = Ok(());
                for &n in &notes {
                    result = result.and(port.send(&[0x90 | channel, n, 100]));
                }
                std::thread::sleep(Duration::from_millis(ms));
                for &n in &notes {
                    result = result.and(port.send(&[0x80 | channel, n, 0]));
                }
                result
            }
            Some("bend") => {
                let value: i32 = words.get(1).and_then(|w| w.parse().ok()).unwrap_or(0);
                let raw = (value.clamp(-8192, 8191) + 8192) as u16;
                port.send(&[0xE0 | channel, (raw & 0x7F) as u8, (raw >> 7) as u8])
            }
            Some("grid") => {
                let bpm: f64 = words.get(1).and_then(|w| w.parse().ok()).unwrap_or(120.0);
                let count = ms_at(2, 8);
                let note = num(3, 60);
                let beat = Duration::from_secs_f64(60.0 / bpm.clamp(20.0, 400.0));
                let start = std::time::Instant::now();
                let mut result = Ok(());
                for i in 0..count {
                    // Sleep to the beat rather than by the beat, so the error does not add up.
                    let due = beat * i as u32;
                    if let Some(wait) = due.checked_sub(start.elapsed()) {
                        std::thread::sleep(wait);
                    }
                    let sent_at = start.elapsed();
                    result = result.and(port.send(&[0x90 | channel, note, 100]));
                    println!("beat {} sent at {:.3} ms", i + 1, sent_at.as_secs_f64() * 1000.0);
                    std::thread::sleep(Duration::from_millis(50));
                    result = result.and(port.send(&[0x80 | channel, note, 0]));
                }
                result
            }
            Some("on") => port.send(&[0x90 | channel, num(1, 36), num(2, 100)]),
            Some("off") => port.send(&[0x80 | channel, num(1, 36), 0]),
            Some("cc") => port.send(&[0xB0 | channel, num(1, 1), num(2, 0)]),
            Some("ch") => {
                channel = num(1, 1).clamp(1, 16) - 1;
                Ok(())
            }
            Some("sleep") => {
                let ms: u64 = words.get(1).and_then(|w| w.parse().ok()).unwrap_or(100);
                std::thread::sleep(Duration::from_millis(ms));
                Ok(())
            }
            Some("quit") => break,
            Some(other) => {
                eprintln!("unknown: {other}");
                Ok(())
            }
            None => Ok(()),
        };
        match sent {
            Ok(()) => println!("ok {line}"),
            Err(e) => println!("error {line}: {e}"),
        }
    }
    drop(port);
    println!("closed: {name}");
}

#[cfg(not(unix))]
fn main() {
    eprintln!("Virtual MIDI ports are only available on macOS and Linux.");
}
