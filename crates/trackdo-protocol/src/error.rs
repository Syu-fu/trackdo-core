//! Errors raised while decoding wire data into core types.

use thiserror::Error;

use trackdo_core::title::{Title, TitleError};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("invalid title: {0}")]
    InvalidTitle(#[from] TitleError),
    /// Every replica must store the exact same value, so titles are not
    /// normalized on receipt.
    #[error("title is not in canonical form")]
    NonCanonicalTitle,
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
