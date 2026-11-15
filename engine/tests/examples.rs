//! Every score shipped in `examples/` compiles with the current compiler.
//!
//! The scores of the old Python prototype live in `examples/legacy-python/` and are skipped on
//! purpose: they use the Ableton format (`ableton_track`, `group`, ...), which this compiler
//! refuses with a message. MIDI profiles (`midi-*.yaml`) are not scores.

use std::fs;
use std::path::Path;

#[test]
fn every_example_score_compiles() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples");
    let mut checked = 0;
    for entry in fs::read_dir(&dir).expect("examples/") {
        let path = entry.expect("entry").path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".yaml") || name.starts_with("midi-") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read");
        if let Err(err) = looper_engine::score::compile_score(&text) {
            panic!("{name} does not compile:\n{err}");
        }
        checked += 1;
    }
    assert!(checked >= 3, "only {checked} example scores found");
}
