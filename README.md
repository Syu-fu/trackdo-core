# trackdo-core

Core library for **trackdo**, a personal app that combines a to-do list with
time tracking, built around seamless offline-first sync across devices.

This repository holds the platform-independent logic shared by every part of
the system:

| Crate | Purpose |
|---|---|
| [`trackdo-core`](crates/trackdo-core) | Domain model and sync logic. Pure Rust with no I/O. |
| [`trackdo-protocol`](crates/trackdo-protocol) | JSON wire format exchanged between clients and the server. |

The sync server and the iOS (UniFFI) / Web (WebAssembly) bindings live in
separate repositories and depend on these crates.

> **Status:** early development. Tasks with a title and a `todo`/`done`
> status sync correctly; time tracking is not implemented yet. APIs and the
> wire format may change before 1.0.

## Design

### Local-first sync

Every edit is applied to local state immediately and queued as an
**operation**. Devices keep working offline and converge once they reconnect.

```
client: command → apply locally → pending queue ──POST /ops──▶ server
                                                                │ assigns seq,
client: apply_remote ◀────────── GET /events (SSE) ─────────────┘ stores, broadcasts
```

- **Conflict resolution** – each task field (`title`, `status`, `deleted`) is
  an independent last-writer-wins register ordered by a
  [Hybrid Logical Clock](https://cse.buffalo.edu/tech-reports/2014-04.pdf).
  Concurrent edits to different fields are both kept, and merging is
  commutative, associative and idempotent.
- **Deletion wins** – deletion is a tombstone that edits to other fields never
  clear.
- **No create operation** – a task comes into existence when the first write
  for its id arrives, so operations may arrive in any order.
- **Delivery** – the server orders operations with a per-user sequence number.
  Clients resume the event stream from their last `seq`, and fall back to a
  full snapshot when the log no longer reaches back that far.
- **Acknowledgement** – pending operations are confirmed by the `POST /ops`
  response, or by receiving them back over the event stream.

### Sans-IO core

`trackdo-core` performs no I/O. Hosts provide the current time and unique ids
through the `Clock` and `IdGen` traits, and persist `ReplicaState` themselves.
This keeps the core deterministic, easy to test, and portable to iOS, the
browser and the server.

## Usage

```rust
use trackdo_core::env::{Clock, IdGen};
use trackdo_core::hlc::DeviceId;
use trackdo_core::op::OpId;
use trackdo_core::replica::Replica;
use trackdo_core::task::TaskId;

use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

struct SystemClock;
impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        since_epoch.as_millis() as u64
    }
}

struct UuidV7;
impl IdGen for UuidV7 {
    fn task_id(&mut self) -> TaskId {
        TaskId(Uuid::now_v7().as_u128())
    }
    fn op_id(&mut self) -> OpId {
        OpId(Uuid::now_v7().as_u128())
    }
}

let device = DeviceId(Uuid::new_v4().as_u128()); // generate once per install and persist
let mut replica = Replica::new(device, SystemClock, UuidV7);
let id = replica.create("Buy milk")?;
replica.complete(id)?;

// Send replica.pending_ops() with POST /ops, then:
//   replica.acknowledge(&accepted_op_ids);
// Feed each server event into:
//   replica.apply_remote(&op, seq)?;
```

Wire types in `trackdo-protocol` convert to and from core types and validate
incoming data:

```json
{
  "op_id": "01928f3a-1b2c-7d4e-8f90-a1b2c3d4e5f6",
  "task_id": "01928f3a-0000-7000-8000-000000000001",
  "hlc": { "wall_ms": 1730000000000, "counter": 3, "device": "01928f39-ffff-7abc-9def-0123456789ab" },
  "change": { "field": "title", "value": "buy milk" }
}
```

## Development

Requires the toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml).

```sh
cargo test --workspace                       # unit, property and convergence tests
cargo clippy --workspace --all-targets -- -D warnings
cargo build --target wasm32-unknown-unknown -p trackdo-core -p trackdo-protocol
cargo llvm-cov --workspace                   # coverage (CI requires >= 80%)
```

Wire-format changes show up as [insta](https://insta.rs) snapshot diffs.
Incompatible changes must bump `PROTOCOL_VERSION`.

## License

[MIT](LICENSE)
