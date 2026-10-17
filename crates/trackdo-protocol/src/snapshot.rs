//! Wire representation of a full state snapshot (`GET /snapshot`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::lww::Lww;
use trackdo_core::op::Seq;
use trackdo_core::task::{Task, TaskId, TaskStatus};

use crate::error::{ProtocolError, decode_title};
use crate::op::{HlcDto, TaskStatusDto};

/// All tasks as of `seq`. `seq` is `null` when the log is still empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDto {
    pub seq: Option<u64>,
    pub tasks: Vec<TaskDto>,
}

/// A task with every field's register, so receivers can merge it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDto {
    pub id: Uuid,
    pub title: Option<RegisterDto<String>>,
    pub status: Option<RegisterDto<TaskStatusDto>>,
    pub deleted: Option<RegisterDto<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDto<T> {
    pub value: T,
    pub hlc: HlcDto,
}

impl SnapshotDto {
    pub fn new(tasks: &[Task], seq: Option<Seq>) -> Self {
        Self {
            seq: seq.map(|Seq(seq)| seq),
            tasks: tasks.iter().map(TaskDto::from).collect(),
        }
    }

    /// Validates and converts into the arguments of `Replica::apply_snapshot`.
    pub fn into_core(self) -> Result<(Vec<Task>, Option<Seq>), ProtocolError> {
        let tasks = self
            .tasks
            .into_iter()
            .map(Task::try_from)
            .collect::<Result<_, _>>()?;
        Ok((tasks, self.seq.map(Seq)))
    }
}

impl From<&Task> for TaskDto {
    fn from(task: &Task) -> Self {
        Self {
            id: Uuid::from_u128(task.id().0),
            title: task
                .title_register()
                .map(|reg| encode(reg, |title| title.as_str().to_owned())),
            status: task
                .status_register()
                .map(|reg| encode(reg, |status| TaskStatusDto::from(*status))),
            deleted: task
                .deleted_register()
                .map(|reg| encode(reg, |deleted| *deleted)),
        }
    }
}

impl TryFrom<TaskDto> for Task {
    type Error = ProtocolError;

    fn try_from(dto: TaskDto) -> Result<Self, Self::Error> {
        let title = dto
            .title
            .map(|reg| Ok::<_, ProtocolError>(Lww::new(decode_title(&reg.value)?, reg.hlc.into())))
            .transpose()?;
        let status = dto
            .status
            .map(|reg| Lww::new(TaskStatus::from(reg.value), reg.hlc.into()));
        let deleted = dto.deleted.map(|reg| Lww::new(reg.value, reg.hlc.into()));
        Ok(Self::from_registers(
            TaskId(dto.id.as_u128()),
            title,
            status,
            deleted,
        ))
    }
}

fn encode<T, U>(register: &Lww<T>, value: impl FnOnce(&T) -> U) -> RegisterDto<U>
where
    T: Clone + Ord,
{
    RegisterDto {
        value: value(register.value()),
        hlc: HlcDto::from(register.hlc()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use trackdo_core::hlc::{DeviceId, Hlc};
    use trackdo_core::task::FieldChange;
    use trackdo_core::title::Title;

    const DEVICE: DeviceId = DeviceId(0x0192_8f39_ffff_7abc_9def_0123_4567_89ab);

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, DEVICE)
    }

    fn title(s: &str) -> Title {
        Title::new(s).unwrap()
    }

    fn full_task() -> Task {
        Task::empty(TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0001))
            .apply(
                &FieldChange::Title(title("buy milk")),
                at(1_730_000_000_000),
            )
            .apply(
                &FieldChange::Status(TaskStatus::Done),
                at(1_730_000_000_500),
            )
            .apply(&FieldChange::Deleted(false), at(1_730_000_000_900))
    }

    fn partial_task() -> Task {
        Task::empty(TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0002)).apply(
            &FieldChange::Status(TaskStatus::Todo),
            at(1_730_000_001_000),
        )
    }

    #[test]
    fn snapshot_wire_format() {
        insta::assert_json_snapshot!(SnapshotDto::new(
            &[full_task(), partial_task()],
            Some(Seq(42))
        ));
    }

    #[test]
    fn empty_snapshot_wire_format() {
        insta::assert_json_snapshot!(SnapshotDto::new(&[], None));
    }

    #[test]
    fn rejects_non_canonical_title_register() {
        let mut dto = TaskDto::from(&full_task());
        dto.title = Some(RegisterDto {
            value: "buy milk ".to_owned(),
            hlc: HlcDto::from(at(1)),
        });

        assert_eq!(Task::try_from(dto), Err(ProtocolError::NonCanonicalTitle));
    }

    fn any_task() -> impl Strategy<Value = Task> {
        let change = prop_oneof![
            "[a-z]{1,8}".prop_map(|s| FieldChange::Title(title(&s))),
            prop::sample::select(vec![TaskStatus::Todo, TaskStatus::Done])
                .prop_map(FieldChange::Status),
            any::<bool>().prop_map(FieldChange::Deleted),
        ];
        let write = (change, any::<u64>(), any::<u32>(), any::<u128>()).prop_map(
            |(change, wall, counter, device)| (change, Hlc::new(wall, counter, DeviceId(device))),
        );
        (any::<u128>(), prop::collection::vec(write, 0..5)).prop_map(|(id, writes)| {
            writes
                .iter()
                .fold(Task::empty(TaskId(id)), |task, (change, hlc)| {
                    task.apply(change, *hlc)
                })
        })
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_snapshot(
            tasks in prop::collection::vec(any_task(), 0..5),
            seq in prop::option::of(any::<u64>().prop_map(Seq)),
        ) {
            let json = serde_json::to_string(&SnapshotDto::new(&tasks, seq)).unwrap();
            let dto: SnapshotDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(dto.into_core(), Ok((tasks, seq)));
        }
    }
}
