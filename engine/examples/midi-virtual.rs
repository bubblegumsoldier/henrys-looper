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
//! note 36        note on (channel 1, velocity 100), note off right after
//! on 36 [vel]    note on only
//! off 36         note off only
//! cc 3 64        controller 3 to 64
//! ch 2           following commands on channel 2
//! sleep 500      wait (milliseconds)
//! quit           close the port and exit (like pulling the cable)
//! ```

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
        let sent = match words.first().copied() {
            Some("note") => {
                let n = num(1, 36);
                let a = port.send(&[0x90 | channel, n, num(2, 100)]);
                std::thread::sleep(Duration::from_millis(30));
                a.and(port.send(&[0x80 | channel, n, 0]))
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
