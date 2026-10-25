//! Errors raised while decoding wire data into core types.

use thiserror::Error;

use trackdo_core::date::DateError;
use trackdo_core::note::{Note, NoteError};
use trackdo_core::title::{Title, TitleError};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("invalid title: {0}")]
    InvalidTitle(#[from] TitleError),
    /// Every replica must store the exact same value, so titles are not
    /// normalized on receipt.
    #[error("title is not in canonical form")]
    NonCanonicalTitle,
    #[error("invalid note: {0}")]
    InvalidNote(#[from] NoteError),
    /// Notes are not normalized on receipt either.
    #[error("note is not in canonical form")]
    NonCanonicalNote,
    #[error("invalid date: {0}")]
    InvalidDate(#[from] DateError),
    #[error("date is not `YYYY-MM-DD`")]
    InvalidDateFormat,
    #[error("event id is not `<log_id>:<seq>`")]
    InvalidEventId,
}

pub(crate) fn decode_title(raw: &str) -> Result<Title, ProtocolError> {
    let title = Title::new(raw)?;
    if title.as_str() != raw {
        return Err(ProtocolError::NonCanonicalTitle);
    }
    Ok(title)
}

pub(crate) fn decode_note(raw: &str) -> Result<Note, ProtocolError> {
    let note = Note::new(raw)?;
    if note.as_str() != raw {
        return Err(ProtocolError::NonCanonicalNote);
    }
    Ok(note)
}
