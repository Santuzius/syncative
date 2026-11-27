//! The types the other layers share, and the port traits through which the engine reaches the database and the remotes. Pure Rust without I/O: no iced, no SeaORM, no Go library (layers: `doc/development/architecture.md`).

pub mod events;
pub mod ports;
pub mod remote;
pub mod run_state;
pub mod sync;
