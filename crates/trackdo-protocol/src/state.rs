//! Stored representation of a replica's state, for hosts to persist between
//! launches.
//!
//! It never goes over the wire, but devices keep it across releases, so its
//! JSON must stay readable just like the wire format.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::op::{LogId, Op, Seq};
use trackdo_core::replica::ReplicaState;
use trackdo_core::task::Task;
use trackdo_core::time_entry::TimeEntry;

use crate::error::ProtocolError;
use crate::op::{HlcDto, OpDto};
use crate::snapshot::{TaskDto, TimeEntryDto};

/// Everything needed to restore a replica, including its device id in
/// `hlc.device`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaStateDto {
    pub hlc: HlcDto,
    pub tasks: Vec<TaskDto>,
    pub time_entries: Vec<TimeEntryDto>,
    pub pending: Vec<OpDto>,
    pub log_id: Option<Uuid>,
    pub last_seq: Option<u64>,
}

impl From<&ReplicaState> for ReplicaStateDto {
    fn from(state: &ReplicaState) -> Self {
        Self {
            hlc: HlcDto::from(state.hlc),
            tasks: state.tasks.iter().map(TaskDto::from).collect(),
            time_entries: state.time_entries.iter().map(TimeEntryDto::from).collect(),
            pending: state.pending.iter().map(OpDto::from).collect(),
            log_id: state.log_id.map(|LogId(id)| Uuid::from_u128(id)),
            last_seq: state.last_seq.map(|Seq(seq)| seq),
        }
    }
}

/// Validates stored data the same way as data received from the server.
impl TryFrom<ReplicaStateDto> for ReplicaState {
    type Error = ProtocolError;

    fn try_from(dto: ReplicaStateDto) -> Result<Self, Self::Error> {
        Ok(Self {
            hlc: dto.hlc.into(),
            tasks: dto
                .tasks
                .into_iter()
                .map(Task::try_from)
                .collect::<Result<_, _>>()?,
            time_entries: dto.time_entries.into_iter().map(TimeEntry::from).collect(),
            pending: dto
                .pending
                .into_iter()
                .map(Op::try_from)
                .collect::<Result<_, _>>()?,
            log_id: dto.log_id.map(|id| LogId(id.as_u128())),
            last_seq: dto.last_seq.map(Seq),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use trackdo_core::hlc::{DeviceId, Hlc};
    use trackdo_core::op::OpId;
    use trackdo_core::task::{TaskChange, TaskId};
    use trackdo_core::title::Title;

    use crate::op::tests::{any_entry_op, any_task_op};
    use crate::snapshot::RegisterDto;
    use crate::snapshot::tests::{any_entry, any_hlc, any_task, full_task, running_entry};

    const DEVICE: DeviceId = DeviceId(0x0192_8f39_ffff_7abc_9def_0123_4567_89ab);

    fn pending_op() -> Op {
        Op::task(
            OpId(0x0192_8f3a_1b2c_7d4e_8f90_a1b2_c3d4_e5f6),
            TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0001),
            Hlc::new(1_730_000_500_000, 0, DEVICE),
            TaskChange::Title(Title::new("buy oat milk").unwrap()),
        )
    }

    fn state() -> ReplicaState {
        ReplicaState {
            hlc: Hlc::new(1_730_000_500_000, 0, DEVICE),
            tasks: vec![full_task()],
            time_entries: vec![running_entry()],
            pending: vec![pending_op()],
            log_id: Some(LogId(0x0192_8f30_0000_7000_8000_0000_0000_0001)),
            last_seq: Some(Seq(42)),
        }
    }

    #[test]
    fn replica_state_stored_format() {
        insta::assert_json_snapshot!(ReplicaStateDto::from(&state()));
    }

    #[test]
    fn fresh_replica_state_stored_format() {
        insta::assert_json_snapshot!(ReplicaStateDto::from(&ReplicaState {
            hlc: Hlc::zero(DEVICE),
            tasks: Vec::new(),
            time_entries: Vec::new(),
            pending: Vec::new(),
            log_id: None,
            last_seq: None,
        }));
    }

    #[test]
    fn rejects_non_canonical_title_in_tasks() {
        let mut dto = ReplicaStateDto::from(&state());
        dto.tasks[0].title = Some(RegisterDto {
            value: " buy milk".to_owned(),
            hlc: dto.hlc,
        });

        assert_eq!(
            ReplicaState::try_from(dto),
            Err(ProtocolError::NonCanonicalTitle)
        );
    }

    #[test]
    fn rejects_non_canonical_title_in_pending() {
        let mut json = serde_json::to_value(ReplicaStateDto::from(&state())).unwrap();
        json["pending"][0]["target"]["change"]["value"] = "buy oat milk ".into();
        let dto: ReplicaStateDto = serde_json::from_value(json).unwrap();

        assert_eq!(
            ReplicaState::try_from(dto),
            Err(ProtocolError::NonCanonicalTitle)
        );
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_replica_state(
            hlc in any_hlc(),
            tasks in prop::collection::vec(any_task(), 0..5),
            time_entries in prop::collection::vec(any_entry(), 0..5),
            pending in prop::collection::vec(prop_oneof![any_task_op(), any_entry_op()], 0..5),
            log_id in prop::option::of(any::<u128>().prop_map(LogId)),
            last_seq in prop::option::of(any::<u64>().prop_map(Seq)),
        ) {
            let state = ReplicaState { hlc, tasks, time_entries, pending, log_id, last_seq };
            let json = serde_json::to_string(&ReplicaStateDto::from(&state)).unwrap();
            let dto: ReplicaStateDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(ReplicaState::try_from(dto), Ok(state));
        }
    }
}
