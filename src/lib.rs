//! Telex core library.
//!
//! Layers:
//! - `domain`: envelope, address, enums, validation, canonical form, dedupe. No I/O.
//! - `store`: maildir-style spool, roles, tokens and bindings.
//! - `index`: derived SQLite index over the spool, rebuildable at any time.
//! - `protocol`: local socket request/response types.
//! - `client`: CLI side of the protocol.
//! - `maild`: local daemon.
//! - `cli`: command line entry point.

pub mod cli;
pub mod client;
pub mod domain;
pub mod index;
pub mod maild;
pub mod protocol;
pub mod store;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
