//! Ids of server events (`GET /events`), which tell where to resume.

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use trackdo_core::op::{LogId, Seq};

use crate::error::ProtocolError;

/// The SSE `id` of an event, sent back as `Last-Event-ID` to resume:
/// `<log_id>:<seq>`, e.g. `01928f30-0000-7000-8000-000000000001:42`.
///
/// Carrying the log lets the server refuse to resume from a position in a
/// log it no longer has, so the client resyncs from a snapshot instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId {
    pub log_id: LogId,
    pub seq: Seq,
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            log_id: LogId(log_id),
            seq: Seq(seq),
        } = self;
        write!(f, "{}:{seq}", Uuid::from_u128(*log_id))
    }
}

impl FromStr for EventId {
    type Err = ProtocolError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (log_id, seq) = raw.rsplit_once(':').ok_or(ProtocolError::InvalidEventId)?;
        let log_id = Uuid::try_parse(log_id).map_err(|_| ProtocolError::InvalidEventId)?;
        if seq.is_empty() || !seq.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ProtocolError::InvalidEventId);
        }
        let seq = seq.parse().map_err(|_| ProtocolError::InvalidEventId)?;
        Ok(Self {
            log_id: LogId(log_id.as_u128()),
            seq: Seq(seq),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const LOG: LogId = LogId(0x0192_8f30_0000_7000_8000_0000_0000_0001);

    #[test]
    fn formats_as_log_id_and_seq() {
        let id = EventId {
            log_id: LOG,
            seq: Seq(42),
        };

        assert_eq!(id.to_string(), "01928f30-0000-7000-8000-000000000001:42");
    }

    #[test]
    fn parses_its_own_format() {
        let parsed = "01928f30-0000-7000-8000-000000000001:42".parse();

        assert_eq!(
            parsed,
            Ok(EventId {
                log_id: LOG,
                seq: Seq(42),
            })
        );
    }

    #[test]
    fn rejects_malformed_ids() {
        for raw in [
            "",
            "42",
            "01928f30-0000-7000-8000-000000000001",
            "01928f30-0000-7000-8000-000000000001:",
            "01928f30-0000-7000-8000-000000000001:-1",
            "01928f30-0000-7000-8000-000000000001:4 2",
            "not-a-uuid:42",
            ":42",
        ] {
            assert_eq!(
                raw.parse::<EventId>(),
                Err(ProtocolError::InvalidEventId),
                "{raw:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn roundtrips_through_its_string(log in any::<u128>(), seq in any::<u64>()) {
            let id = EventId { log_id: LogId(log), seq: Seq(seq) };

            prop_assert_eq!(id.to_string().parse(), Ok(id));
        }
    }
}
