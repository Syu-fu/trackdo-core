//! Validated free-form task note.

use thiserror::Error;

/// Maximum note length, counted in Unicode scalar values.
pub const MAX_NOTE_CHARS: usize = 10_000;

/// Multi-line text of at most [`MAX_NOTE_CHARS`], with `\n` line breaks and
/// no trailing whitespace. Empty means the task has no note.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Note(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NoteError {
    #[error("note must be at most {max} characters, got {actual}")]
    TooLong { max: usize, actual: usize },
}

impl Note {
    pub fn new(input: &str) -> Result<Self, NoteError> {
        let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
        let trimmed = normalized.trim_end();
        let len = trimmed.chars().count();
        if len > MAX_NOTE_CHARS {
            return Err(NoteError::TooLong {
                max: MAX_NOTE_CHARS,
                actual: len,
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_inner_lines_and_leading_indentation() {
        let note = Note::new("  - milk\n  - eggs").unwrap();

        assert_eq!(note.as_str(), "  - milk\n  - eggs");
    }

    #[test]
    fn normalizes_line_breaks_to_line_feed() {
        let note = Note::new("a\r\nb\rc").unwrap();

        assert_eq!(note.as_str(), "a\nb\nc");
    }

    #[test]
    fn trims_trailing_whitespace() {
        let note = Note::new("a\n\n \t").unwrap();

        assert_eq!(note.as_str(), "a");
    }

    #[test]
    fn whitespace_only_input_is_empty() {
        let note = Note::new(" \n\t").unwrap();

        assert!(note.is_empty());
        assert_eq!(note, Note::default());
    }

    #[test]
    fn accepts_max_length_counted_in_chars() {
        let input = "あ".repeat(MAX_NOTE_CHARS);

        assert!(Note::new(&input).is_ok());
    }

    #[test]
    fn rejects_input_over_max_length() {
        let input = "a".repeat(MAX_NOTE_CHARS + 1);

        assert_eq!(
            Note::new(&input),
            Err(NoteError::TooLong {
                max: MAX_NOTE_CHARS,
                actual: MAX_NOTE_CHARS + 1,
            })
        );
    }

    #[test]
    fn length_is_checked_after_normalizing() {
        let input = format!("{}\r\n", "a".repeat(MAX_NOTE_CHARS));

        assert!(Note::new(&input).is_ok());
    }
}
