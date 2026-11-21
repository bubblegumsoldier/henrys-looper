//! The sheet: lyrics, chords and stage directions that run along with the song in the Live view.
//!
//! ```text
//!   sheet: |                         score text (a YAML block scalar, kept byte for byte)
//!     beats per chord: 2
//!     # Verse 1 @verse_1             part header, anchored to the start of section `verse_1`
//!     > REC bass · 2 bars |2         stage direction (REC = recording), takes 2 bars
//!     Am            C/G              chords above the text, at their columns
//!     First line of the verse
//!     [F]Second line with [C]inline chords |1.5
//! ```
//!
//! This is the one parser of the format. The app does not parse a sheet itself: it gets the
//! [`CompiledSheet`] with the compiled score (`CompiledScore::sheet`) and only does arithmetic on it
//! (which line is current at the runner's position). The reason it lives here and not in the UI is
//! `score --check`: it names unknown anchors with their line and warns when the text between two
//! anchors does not last as long as the sections it belongs to - both need the whole timing.
//!
//! # Timing
//!
//! The sheet has its own timeline in bars, from 0. A line lasts `chords × beats per chord` beats
//! (2 per chord unless `beats per chord: N` says otherwise), or `N` bars with a `|N` suffix
//! (decimals allowed); a line with neither lasts one bar. A stage direction lasts nothing unless it
//! has a suffix itself. Beats become bars with the score's time signature.
//!
//! # Sections
//!
//! `# Name @section_id` anchors a part to the start of that section. Every section gets a *span*
//! of the sheet's timeline ([`SheetSpan`]): an anchored one starts at its anchor, an unanchored one
//! runs on from the section before it (one pass), and a span ends where the next section's begins.
//! Without any anchor that is simply the sections one after the other from the top. The Live view
//! maps the runner's position into the current section's span - linearly, or wrapping around in a
//! section that loops until Release - which is what keeps the text in step across Release.
//!
//! Nothing here runs on the audio thread, and nothing the runner does depends on it.

use serde::{Deserialize, Serialize};

/// Beats per chord when the sheet does not say.
pub const DEFAULT_BEATS_PER_CHORD: f64 = 2.0;

/// Tab stops when a line is laid out in columns.
const TAB_WIDTH: usize = 4;

/// A chord and the column it stands on (in characters, from the start of the line).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetChord {
    pub name: String,
    pub col: u32,
}

/// One element of the sheet, in written order. `line` is 1-based inside the sheet text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SheetItem {
    /// `# Name` or `# Name @section_id`. `at` is the bar where the part begins.
    Part {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section: Option<String>,
        at: f64,
        line: u32,
    },
    /// `> text`: a stage direction. In force from `start`; lasts `bars` (0 unless written).
    Direction {
        text: String,
        /// Starts with `REC`: something is recorded.
        rec: bool,
        bars: f64,
        start: f64,
        line: u32,
    },
    /// A line of text with its chords (or only chords).
    Line {
        text: String,
        chords: Vec<SheetChord>,
        bars: f64,
        start: f64,
        line: u32,
    },
}

impl SheetItem {
    pub fn line(&self) -> u32 {
        match self {
            SheetItem::Part { line, .. }
            | SheetItem::Direction { line, .. }
            | SheetItem::Line { line, .. } => *line,
        }
    }
}

/// The stretch of the sheet's timeline that belongs to one section: `[start, end)` in bars.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetSpan {
    pub start: f64,
    pub end: f64,
}

/// The parsed sheet, as the app gets it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledSheet {
    pub items: Vec<SheetItem>,
    /// Length of the whole sheet in bars.
    pub total: f64,
    /// One per section of the score, in section order.
    pub spans: Vec<SheetSpan>,
}

/// What the spans and the checks need to know about a section.
#[derive(Clone, Debug)]
pub struct SectionInfo<'a> {
    pub id: &'a str,
    pub bars: u32,
    pub autorelease: bool,
}

/// A problem or a hint about the sheet; `line` is 1-based inside the sheet text.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetIssue {
    pub line: u32,
    pub message: String,
    pub suggestion: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// Parse a sheet. Never fails: every line is something (a line of text, at worst).
pub fn parse_sheet(text: &str, beats_per_bar: u32) -> (Vec<SheetItem>, f64) {
    /// A line of chords waiting for the text below it.
    struct Pending {
        chords: Vec<SheetChord>,
        over: Option<f64>,
        line: u32,
        beats_per_chord: f64,
    }
    struct Out {
        items: Vec<SheetItem>,
        bar: f64,
        beats_per_bar: f64,
    }
    impl Out {
        fn line(&mut self, text: String, chords: Vec<SheetChord>, over: Option<f64>, beats_per_chord: f64, line: u32) {
            let bars = match over {
                Some(bars) => bars,
                None if !chords.is_empty() => chords.len() as f64 * beats_per_chord / self.beats_per_bar,
                None => 1.0,
            };
            self.items.push(SheetItem::Line { text, chords, bars, start: self.bar, line });
            self.bar += bars;
        }
        /// A chord line nobody wrote text under stands on its own.
        fn flush(&mut self, pending: &mut Option<Pending>) {
            if let Some(p) = pending.take() {
                self.line(String::new(), p.chords, p.over, p.beats_per_chord, p.line);
            }
        }
    }

    let mut out = Out { items: Vec::new(), bar: 0.0, beats_per_bar: f64::from(beats_per_bar.max(1)) };
    let mut beats_per_chord = DEFAULT_BEATS_PER_CHORD;
    let mut pending: Option<Pending> = None;

    for (n, source) in text.split('\n').enumerate() {
        let line = n as u32 + 1;
        let expanded = expand_tabs(source.trim_end_matches('\r'));
        let (raw, over) = split_suffix(expanded.trim_end());
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = beats_per_chord_setting(trimmed) {
            beats_per_chord = value;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            out.flush(&mut pending);
            let (name, section) = split_anchor(rest.trim());
            out.items.push(SheetItem::Part { name, section, at: out.bar, line });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            out.flush(&mut pending);
            let text = rest.trim().to_string();
            let bars = over.unwrap_or(0.0);
            let rec = is_rec(&text);
            out.items.push(SheetItem::Direction { text, rec, bars, start: out.bar, line });
            out.bar += bars;
            continue;
        }
        if trimmed.split_whitespace().all(is_chord) {
            // A line of chords: their columns are kept for the text below.
            out.flush(&mut pending);
            pending = Some(Pending { chords: chord_columns(raw), over, line, beats_per_chord });
            continue;
        }
        if let Some((text, chords)) = inline_chords(raw) {
            out.flush(&mut pending);
            out.line(text, chords, over, beats_per_chord, line);
            continue;
        }
        match pending.take() {
            Some(p) => out.line(raw.to_string(), p.chords, over.or(p.over), p.beats_per_chord, line),
            None => out.line(raw.to_string(), Vec::new(), over, beats_per_chord, line),
        }
    }
    out.flush(&mut pending);
    (out.items, out.bar)
}

/// Tabs to spaces, so columns mean the same in a chord line and the text below it.
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let next = (col / TAB_WIDTH + 1) * TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', next - col));
            col = next;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// `text |1.5` -> (`text`, Some(1.5)). The number is a plain decimal; anything else after a `|`
/// is text.
fn split_suffix(line: &str) -> (&str, Option<f64>) {
    let Some(bar) = line.rfind('|') else {
        return (line, None);
    };
    let number = line[bar + 1..].trim();
    let mut parts = number.splitn(2, '.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || fraction.is_some_and(|f| !digits(f)) {
        return (line, None);
    }
    match number.parse::<f64>() {
        Ok(value) if value.is_finite() => (line[..bar].trim_end(), Some(value)),
        _ => (line, None),
    }
}

/// `beats per chord: 3` (any case) -> 3. Zero and nonsense are not a setting.
fn beats_per_chord_setting(line: &str) -> Option<f64> {
    let lower = line.to_ascii_lowercase();
    let rest = lower.strip_prefix("beats per chord")?.trim_start().strip_prefix(':')?.trim();
    let value: f64 = rest.parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

/// `Verse 1 @verse_1` -> (`Verse 1`, Some(`verse_1`)).
fn split_anchor(header: &str) -> (String, Option<String>) {
    if let Some((name, last)) = header.rsplit_once(char::is_whitespace).or(Some(("", header)))
        && let Some(id) = last.strip_prefix('@')
        && !id.is_empty()
    {
        return (name.trim().to_string(), Some(id.to_string()));
    }
    (header.to_string(), None)
}

/// Starts with the word `REC` (any case).
fn is_rec(text: &str) -> bool {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(3).collect();
    head.eq_ignore_ascii_case("rec") && chars.next().is_none_or(|c| !c.is_alphanumeric() && c != '_')
}

/// A chord symbol as tab sites write them: `Am`, `C/G`, `F#m7`, `Bbmaj7`, `G9sus4`, `CM7`,
/// `C#m7b5`. A root `A`–`G` with an optional `#`/`b`, then only the words and signs chord names
/// are made of (so `Chorus` or `Bad` stay words), optionally `/` and a bass note.
pub fn is_chord(word: &str) -> bool {
    const PARTS: [&str; 17] = [
        "maj", "min", "dim", "aug", "sus", "add", "alt", "m", "M", "b", "#", "+", "-", "°", "ø", "(", ")",
    ];
    fn quality_ok(mut rest: &str) -> bool {
        while !rest.is_empty() {
            if let Some(stripped) = rest.strip_prefix(|c: char| c.is_ascii_digit()) {
                rest = stripped;
                continue;
            }
            match PARTS.iter().find(|p| rest.starts_with(**p)) {
                Some(part) => rest = &rest[part.len()..],
                None => return false,
            }
        }
        true
    }
    let (main, bass) = match word.split_once('/') {
        Some((main, bass)) => (main, Some(bass)),
        None => (word, None),
    };
    fn root(s: &str) -> Option<&str> {
        let mut chars = s.chars();
        if !matches!(chars.next()?, 'A'..='G') {
            return None;
        }
        let rest = chars.as_str();
        Some(rest.strip_prefix(['#', 'b']).unwrap_or(rest))
    }
    let Some(quality) = root(main) else { return false };
    if !quality_ok(quality) {
        return false;
    }
    match bass {
        None => true,
        Some(bass) => root(bass).is_some_and(str::is_empty),
    }
}

/// The chords of a chord line, each with the column it starts on.
fn chord_columns(line: &str) -> Vec<SheetChord> {
    let mut chords = Vec::new();
    let mut col = 0u32;
    let mut word = String::new();
    let mut start = 0u32;
    for c in line.chars() {
        if c.is_whitespace() {
            if !word.is_empty() {
                chords.push(SheetChord { name: std::mem::take(&mut word), col: start });
            }
        } else {
            if word.is_empty() {
                start = col;
            }
            word.push(c);
        }
        col += 1;
    }
    if !word.is_empty() {
        chords.push(SheetChord { name: word, col: start });
    }
    chords
}

/// `[Am]Call me [C/G]up` -> (`Call me up`, [Am@0, C/G@8]). `None` when the line has no `[chord]`;
/// brackets around anything else stay text.
fn inline_chords(line: &str) -> Option<(String, Vec<SheetChord>)> {
    let mut text = String::new();
    let mut chords = Vec::new();
    let mut col = 0u32;
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let before = &rest[..open];
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let name = &after[..close];
        text.push_str(before);
        col += before.chars().count() as u32;
        if is_chord(name) {
            chords.push(SheetChord { name: name.to_string(), col });
        } else {
            let literal = &rest[open..open + close + 2];
            text.push_str(literal);
            col += literal.chars().count() as u32;
        }
        rest = &after[close + 1..];
    }
    if chords.is_empty() {
        return None;
    }
    text.push_str(rest);
    Some((text, chords))
}

// ---------------------------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------------------------

/// Parse and lay the sheet over the sections. Anchors that name no section, or that stand out of
/// order, are ignored here; [`check_sheet`] reports them.
pub fn compile_sheet(text: &str, beats_per_bar: u32, sections: &[SectionInfo<'_>]) -> CompiledSheet {
    let (items, total) = parse_sheet(text, beats_per_bar);
    let spans = spans(&items, total, sections);
    CompiledSheet { items, total, spans }
}

/// The valid anchors in sheet order: (section index, bar).
fn anchors(items: &[SheetItem], sections: &[SectionInfo<'_>]) -> Vec<(usize, f64)> {
    let mut out: Vec<(usize, f64)> = Vec::new();
    for item in items {
        let SheetItem::Part { section: Some(id), at, .. } = item else { continue };
        let Some(index) = sections.iter().position(|s| s.id == id) else { continue };
        if out.last().is_some_and(|(last, _)| index <= *last) {
            continue;
        }
        out.push((index, *at));
    }
    out
}

fn spans(items: &[SheetItem], total: f64, sections: &[SectionInfo<'_>]) -> Vec<SheetSpan> {
    let anchors = anchors(items, sections);
    let mut starts: Vec<f64> = Vec::with_capacity(sections.len());
    for index in 0..sections.len() {
        let start = match anchors.iter().find(|(i, _)| *i == index) {
            Some((_, at)) => *at,
            None => {
                let run_on = match index {
                    0 => 0.0,
                    _ => starts[index - 1] + f64::from(sections[index - 1].bars),
                };
                // Never past the next anchor: the text there belongs to its own section.
                match anchors.iter().find(|(i, _)| *i > index) {
                    Some((_, next)) => run_on.min(*next),
                    None => run_on,
                }
            }
        };
        starts.push(start);
    }
    (0..sections.len())
        .map(|index| {
            let start = starts[index];
            let end = match starts.get(index + 1) {
                Some(next) => next.max(start),
                None => total.max(start),
            };
            SheetSpan { start, end }
        })
        .collect()
}

/// Errors (anchors that name no section, stand out of order or twice) and notes (text between two
/// anchors that lasts longer or shorter than its sections).
pub fn check_sheet(sheet: &CompiledSheet, sections: &[SectionInfo<'_>]) -> (Vec<SheetIssue>, Vec<SheetIssue>) {
    let ids: Vec<&str> = sections.iter().map(|s| s.id).collect();
    let mut errors = Vec::new();
    let mut notes = Vec::new();
    // (section index, bar, line, header) of every valid anchor, in sheet order.
    let mut valid: Vec<(usize, f64, u32, String)> = Vec::new();
    for item in &sheet.items {
        let SheetItem::Part { name, section: Some(id), at, line } = item else { continue };
        let Some(index) = ids.iter().position(|s| s == id) else {
            errors.push(SheetIssue {
                line: *line,
                message: format!("Sheet: '@{id}' names no section of the score."),
                suggestion: Some(match super::error::best_match(id, &ids) {
                    Some(hit) => format!("did you mean '@{hit}'?"),
                    None => super::error::allowed(&ids),
                }),
            });
            continue;
        };
        if let Some((last, _, last_line, _)) = valid.iter().find(|(i, ..)| *i >= index) {
            let message = if *last == index {
                format!("Sheet: section '{id}' is anchored twice (also on sheet line {last_line}).")
            } else {
                format!(
                    "Sheet: '@{id}' stands after '@{}' on the sheet, but section '{id}' comes first in the score.",
                    ids[*last]
                )
            };
            errors.push(SheetIssue {
                line: *line,
                message,
                suggestion: Some("Anchors follow the order of the sections, each section at most once.".to_string()),
            });
            continue;
        }
        let header = if name.is_empty() { format!("@{id}") } else { format!("# {name}") };
        valid.push((index, *at, *line, header));
    }
    if valid.is_empty() {
        return (errors, notes);
    }

    // The text before the first anchor belongs to the sections before it.
    if let Some((first, at, line, header)) = valid.first()
        && *first > 0
    {
        let expected: u32 = sections[..*first].iter().map(|s| s.bars).sum();
        if (at - f64::from(expected)).abs() > 1e-6 {
            notes.push(SheetIssue {
                line: *line,
                message: format!(
                    "Sheet: {header} begins at bar {} of the text, but section '{}' starts after {expected} bars.",
                    fmt_bars(*at),
                    ids[*first]
                ),
                suggestion: None,
            });
        }
    }
    for (k, (index, at, line, header)) in valid.iter().enumerate() {
        let (until, end) = match valid.get(k + 1) {
            Some((next, next_at, ..)) => (*next, *next_at),
            None => (sections.len(), sheet.total),
        };
        let range = &sections[*index..until];
        let actual = end - at;
        let names: Vec<&str> = range.iter().map(|s| s.id).collect();
        if let [single] = range
            && !single.autorelease
        {
            // A section that loops until Release: the text wraps, so it should be whole passes.
            let passes = actual / f64::from(single.bars.max(1));
            if actual > 1e-6 && (passes - passes.round()).abs() < 1e-6 {
                continue;
            }
            notes.push(SheetIssue {
                line: *line,
                message: format!(
                    "Sheet: the text of {header} lasts {} bar{}, section '{}' loops {} bar{} at a time - it is not a whole number of passes.",
                    fmt_bars(actual),
                    plural(actual),
                    single.id,
                    single.bars,
                    plural(f64::from(single.bars))
                ),
                suggestion: None,
            });
            continue;
        }
        let expected: u32 = range.iter().map(|s| s.bars).sum();
        if (actual - f64::from(expected)).abs() > 1e-6 {
            notes.push(SheetIssue {
                line: *line,
                message: format!(
                    "Sheet: the text of {header} lasts {} bar{}, {} {} {expected} bar{}.",
                    fmt_bars(actual),
                    plural(actual),
                    match names.len() {
                        1 => format!("section '{}'", names[0]),
                        _ => format!("sections '{}' to '{}'", names[0], names[names.len() - 1]),
                    },
                    if names.len() == 1 { "has" } else { "have" },
                    plural(f64::from(expected))
                ),
                suggestion: None,
            });
        }
    }
    (errors, notes)
}

/// `7.5`, `8`, `1.063` - at most three decimals, no trailing zeros.
fn fmt_bars(bars: f64) -> String {
    let text = format!("{bars:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    text.to_string()
}

fn plural(value: f64) -> &'static str {
    if (value - 1.0).abs() < 1e-9 { "" } else { "s" }
}

#[cfg(test)]
mod tests;
