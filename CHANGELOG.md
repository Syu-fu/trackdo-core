# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/Syu-fu/trackdo-core/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Syu-fu/trackdo-core/releases/tag/v0.1.0
