//! Framework-agnostic domain types.
//!
//! Everything in this module is pure Rust — no GTK, no SeaORM, no Go FFI.
//! Infrastructure adapters map between these types and their external
//! representations; services work only with these types.

pub mod events;
pub mod ports;
pub mod remote;
pub mod run_state;
pub mod sync;
