//! Compiler diagnostics: German, positioned, and collected rather than thrown one at a time.
//!
//! Three properties matter, in this order:
//!
//! 1. **All of them at once.** Writing a score is editing a text file; a compiler that stops at the
//!    first problem turns one pass into five. [`ScoreError`] therefore carries a list.
//! 2. **A position.** [`ScoreIssue::line`] / [`ScoreIssue::column`] come from the marked YAML tree
//!    and point at the offending key, not at the block around it.
//! 3. **A way out.** [`ScoreIssue::suggestion`] holds the "meintest du 'bass'?" or "erlaubt ist
//!    ..." half of the message. It is a separate field so a UI can render it differently.
//!
//! The shape matches `docs/contracts-v0.md`: `{"line", "column", "message", "suggestion"}`.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::yaml::Pos;

/// Below this similarity a "did you mean" would be noise rather than help. 0.5 on the normalised
/// Damerau-Levenshtein scale means "at most half the characters differ", which accepts `bss` for
/// `bass` (0.75) and rejects `x` for `gitarre` (0.14).
const SUGGESTION_CUTOFF: f64 = 0.5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreIssue {
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
    pub suggestion: Option<String>,
}

impl ScoreIssue {
    pub fn new(pos: Option<Pos>, message: impl Into<String>) -> Self {
        Self {
            line: pos.map(|p| p.line),
            column: pos.map(|p| p.column),
            message: message.into(),
            suggestion: None,
        }
    }

    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Attach a suggestion only if there is one - lets the caller write
    /// `.maybe_suggestion(suggest(name, known))`.
    #[must_use]
    pub fn maybe_suggestion(mut self, suggestion: Option<String>) -> Self {
        self.suggestion = suggestion;
        self
    }

    fn sort_key(&self) -> (bool, u32, u32) {
        (self.line.is_none(), self.line.unwrap_or(0), self.column.unwrap_or(0))
    }
}

impl fmt::Display for ScoreIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(f, "Zeile {line}, Spalte {column}: ")?,
            (Some(line), None) => write!(f, "Zeile {line}: ")?,
            _ => write!(f, "Partitur: ")?,
        }
        f.write_str(&self.message)?;
        if let Some(suggestion) = &self.suggestion {
            write!(f, " — {suggestion}")?;
        }
        Ok(())
    }
}

/// Everything the compiler found, sorted by position and de-duplicated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreError {
    pub issues: Vec<ScoreIssue>,
}

impl ScoreError {
    pub fn new(mut issues: Vec<ScoreIssue>) -> Self {
        issues.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        issues.dedup_by(|a, b| a.line == b.line && a.column == b.column && a.message == b.message);
        Self { issues }
    }

    pub fn single(issue: ScoreIssue) -> Self {
        Self { issues: vec![issue] }
    }
}

impl fmt::Display for ScoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, issue) in self.issues.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{issue}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ScoreError {}

/// "meintest du 'bass'?" - the closest candidate, if one is close enough.
///
/// Comparison is case-insensitive so that `Bass` still finds `bass`; the similarity itself is
/// `strsim`'s normalised Damerau-Levenshtein, which counts a transposition (`baas`) as one edit
/// rather than two. That matters because transposition is the typo a musician actually makes.
pub fn suggest(word: &str, candidates: &[&str]) -> Option<String> {
    best_match(word, candidates).map(|hit| format!("meintest du '{hit}'?"))
}

/// The bare candidate behind [`suggest`], for callers that want to phrase it differently.
pub fn best_match<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    if word.is_empty() {
        return None;
    }
    let needle = word.to_lowercase();
    let mut best: Option<(f64, &str)> = None;
    for candidate in candidates {
        let score = strsim::normalized_damerau_levenshtein(&needle, &candidate.to_lowercase());
        if score >= SUGGESTION_CUTOFF && best.is_none_or(|(top, _)| score > top) {
            best = Some((score, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// "Erlaubt: a, b, c." - the fallback when nothing is close enough to suggest.
pub fn allowed(candidates: &[&str]) -> String {
    if candidates.is_empty() {
        return "Hier ist kein Wert erlaubt.".to_string();
    }
    format!("Erlaubt: {}.", candidates.join(", "))
}

/// [`suggest`], falling back to the list of allowed values.
pub fn suggest_or_list(word: &str, candidates: &[&str]) -> String {
    suggest(word, candidates).unwrap_or_else(|| allowed(candidates))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_close_typo_is_suggested_and_a_far_one_is_not() {
        assert_eq!(suggest("bss", &["bass", "gitarre"]).as_deref(), Some("meintest du 'bass'?"));
        assert_eq!(suggest("gitarrre", &["gitarre", "voice"]).as_deref(), Some("meintest du 'gitarre'?"));
        assert_eq!(suggest("Voice", &["gitarre", "voice"]).as_deref(), Some("meintest du 'voice'?"));
        assert_eq!(suggest("schlagzeug", &["gitarre", "voice"]), None);
        assert_eq!(suggest("", &["gitarre"]), None);
    }

    #[test]
    fn the_closest_of_several_candidates_wins() {
        assert_eq!(best_match("recrd", &["record", "overdub", "play"]), Some("record"));
        assert_eq!(best_match("ovrdub", &["record", "overdub", "play"]), Some("overdub"));
    }

    #[test]
    fn issues_are_sorted_by_position_and_deduplicated() {
        let err = ScoreError::new(vec![
            ScoreIssue::new(Some(Pos { line: 9, column: 1 }), "spaet"),
            ScoreIssue::new(None, "ohne Position"),
            ScoreIssue::new(Some(Pos { line: 2, column: 7 }), "frueh"),
            ScoreIssue::new(Some(Pos { line: 2, column: 7 }), "frueh"),
        ]);
        let lines: Vec<_> = err.issues.iter().map(|i| i.line).collect();
        assert_eq!(lines, vec![Some(2), Some(9), None]);
    }

    #[test]
    fn the_rendered_message_names_line_and_column() {
        let issue = ScoreIssue::new(Some(Pos { line: 12, column: 5 }), "Track 'bss' ist unbekannt.")
            .with_suggestion("meintest du 'bass'?");
        assert_eq!(
            issue.to_string(),
            "Zeile 12, Spalte 5: Track 'bss' ist unbekannt. — meintest du 'bass'?"
        );
    }
}
