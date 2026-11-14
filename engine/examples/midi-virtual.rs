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
//! note 36        Note an (Kanal 1, Anschlag 100), gleich danach Note aus
//! on 36 [vel]    nur Note an
//! off 36         nur Note aus
//! cc 3 64        Regler 3 auf 64
//! ch 2           folgende Befehle auf Kanal 2
//! sleep 500      warten (Millisekunden)
//! quit           Port schliessen und beenden (wie Kabel ab)
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
    let output = MidiOutput::new("Looper Testcontroller").expect("MIDI-System");
    let mut port = output
        .create_virtual(&name)
        .unwrap_or_else(|e| panic!("virtueller Port \"{name}\": {e}"));
    println!("bereit: {name}");

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
                eprintln!("unbekannt: {other}");
                Ok(())
            }
            None => Ok(()),
        };
        match sent {
            Ok(()) => println!("ok {line}"),
            Err(e) => println!("fehler {line}: {e}"),
        }
    }
    drop(port);
    println!("geschlossen: {name}");
}

#[cfg(not(unix))]
fn main() {
    eprintln!("Virtuelle MIDI-Ports gibt es hier nur unter macOS und Linux.");
}
