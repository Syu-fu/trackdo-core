# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Replica::reject` drops pending operations the server refused for good, so
  one invalid operation no longer blocks every later push. The rejected
  writes stay applied locally until a newer write to the same field arrives.

## [0.2.0] - 2026-10-18

### Added

- `TimeEntry` with independent `started_at`, `stopped_at` and `deleted`
  registers, plus `duration` and `needs_attention` for entries whose stop
  time precedes their start time after concurrent edits.
- Time tracking commands on `Replica`: `start`, `stop`, `edit_times`,
  `delete_entry` and `add_entry`. Starting a timer stops the running one,
  times may not lie in the future, and an entry never stops before it starts.
- Completing or deleting a task stops its running timer, and starting a timer
  on a done task reopens it.
- Read API for time tracking: `running_entry`, `is_in_progress`,
  `entries_for`, `tracked_entries` and `task_total`, returning
  `TimeEntryView`s. When syncing leaves several timers running, the one
  started last wins and the others are shown as stopped when it started; a
  timer running on a deleted task is shown as stopped when the task was
  deleted.
- Replicas write such derived stops back as operations after applying remote
  operations or snapshots, so stored state catches up with what is shown.
- Time entry operations on the wire:
  `"target": { "type": "time_entry", "entry_id", "task_id", "change" }`.
- Snapshots carry time entries, both in core (`Snapshot::time_entries`) and
  on the wire (`"time_entries"`, with every field's register). `TimeEntry`
  exposes its registers and can be rebuilt with `from_registers`.

### Changed

- **Breaking:** `Op` now carries an `OpTarget` naming the entity it writes to,
  so operations can address entities other than tasks. `FieldChange` is
  renamed to `TaskChange`.
- **Breaking:** wire format of operations nests the task id and change under
  `"target": { "type": "task", ... }`. `PROTOCOL_VERSION` is now 2.
- **Breaking:** `IdGen` requires `time_entry_id`, and `ReplicaState` gains
  `time_entries`.
- **Breaking:** `Replica::apply_snapshot` takes a `Snapshot`, and rejects one
  that moves a known time entry to another task. `SnapshotDto::new` and
  `into_core` are replaced by `From<&Snapshot>` and `TryFrom<SnapshotDto>`, and
  the wire snapshot requires `"time_entries"`.

## [0.1.0] - 2026-10-17

### Added

- `trackdo-core`
  - Hybrid Logical Clock (`Hlc`) with total ordering and overflow detection.
  - Last-writer-wins register (`Lww`) whose merge is a semilattice join.
  - `Task` aggregate with independent `title`, `status` and `deleted` registers,
    field-wise merge and deletion as a tombstone.
  - Validated `Title` (trimmed, single line, 1–500 characters).
  - `Replica` with `create` / `rename` / `complete` / `reopen` / `delete`
    commands, remote operation delivery, acknowledgement, snapshots and
    state export/restore.
  - `Clock` and `IdGen` traits so hosts inject time and ids.
- `trackdo-protocol`
  - JSON wire format for operations and snapshots, with validation on decode.
  - `PROTOCOL_VERSION` = 1.

[Unreleased]: https://github.com/Syu-fu/trackdo-core/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Syu-fu/trackdo-core/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Syu-fu/trackdo-core/releases/tag/v0.1.0
