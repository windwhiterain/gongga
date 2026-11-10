//! One channel's program, and the two files the server talks to it with.
//!
//! # Invariant
//!
//! This is its own program, not the server started again under a flag. What it owes the
//! channel is a process that outlives the server -- it holds the child's stdin, which
//! nobody can re-open, and appends the child's output to a file -- and a second binary
//! with nothing of the server in it is the smallest thing that can pay that debt. It
//! carries no configuration, no database, no network and no command line worth naming:
//! a directory, and the work in it.
//!
//! See `docs/notes/server-channel.md`.

pub mod base64;
pub mod detect;
pub mod job;
pub mod relay;

pub use relay::{Words, run};
