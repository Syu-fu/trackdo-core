//! Wire protocol types shared by trackdo clients and server.
//!
//! DTOs here define the JSON contract. They are converted to and from
//! `trackdo-core` types at the boundary, where incoming data is validated.

pub mod error;
pub mod op;
pub mod snapshot;

/// Version of the sync protocol. Bump on any incompatible wire change.
pub const PROTOCOL_VERSION: u32 = 3;
