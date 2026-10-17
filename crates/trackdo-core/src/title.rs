//! Validated task title.

use thiserror::Error;

/// Maximum title length, counted in Unicode scalar values.
pub const MAX_TITLE_CHARS: usize = 500;

/// A single-line, non-empty, trimmed title of at most [`MAX_TITLE_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Title(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TitleError {
    #[error("title must not be empty")]
    Empty,
    #[error("title must be a single line")]
    ContainsLineBreak,
    #[error("title must be at most {max} characters, got {actual}")]
    TooLong { max: usize, actual: usize },
}

impl Title {
    pub fn new(input: &str) -> Result<Self, TitleError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(TitleError::Empty);
        }
        if trimmed.chars().any(is_line_break) {
            return Err(TitleError::ContainsLineBreak);
        }
        let len = trimmed.chars().count();
        if len > MAX_TITLE_CHARS {
            return Err(TitleError::TooLong {
                max: MAX_TITLE_CHARS,
                actual: len,
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Characters Unicode treats as mandatory line breaks.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_surrounding_whitespace() {
        let title = Title::new("  buy milk \t").unwrap();

        assert_eq!(title.as_str(), "buy milk");
    }

    #[test]
    fn rejects_empty_input() {
        assert_eq!(Title::new(""), Err(TitleError::Empty));
    }

    #[test]
    fn rejects_whitespace_only_input() {
        assert_eq!(Title::new(" \t\n "), Err(TitleError::Empty));
    }

    #[test]
    fn rejects_inner_line_breaks() {
        for input in ["a\nb", "a\rb", "a\u{2028}b", "a\u{2029}b", "a\u{85}b"] {
            assert_eq!(
                Title::new(input),
                Err(TitleError::ContainsLineBreak),
                "{input:?}"
            );
        }
    }

    #[test]
    fn accepts_max_length_counted_in_chars() {
        let input = "あ".repeat(MAX_TITLE_CHARS);

        let title = Title::new(&input).unwrap();

        assert_eq!(title.as_str().chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn rejects_input_over_max_length() {
        let input = "a".repeat(MAX_TITLE_CHARS + 1);

        assert_eq!(
            Title::new(&input),
            Err(TitleError::TooLong {
                max: MAX_TITLE_CHARS,
                actual: MAX_TITLE_CHARS + 1,
            })
        );
    }

    #[test]
    fn length_is_checked_after_trimming() {
        let input = format!("  {}  ", "a".repeat(MAX_TITLE_CHARS));

        assert!(Title::new(&input).is_ok());
    }
}
