//! Wire protocol types shared by trackdo clients and server.
//!
//! DTOs here define the JSON contract. They are converted to and from
//! `trackdo-core` types at the boundary, where incoming data is validated.
//! [`state`] holds the form hosts store locally instead; it never goes over
//! the wire.

pub mod error;
pub mod event;
pub mod op;
pub mod snapshot;
pub mod state;

/// Version of the sync protocol. Bump on any incompatible wire change.
pub const PROTOCOL_VERSION: u32 = 3;
