//! One machine's shells, driven over a stdio runner process.
//!
//! See `docs/notes/server-remote.md`.

pub mod endpoint;
pub mod files;
pub mod pick;
pub mod protocol;
pub mod runner;
pub mod transport;

pub use endpoint::{Endpoint, Reach};
pub use files::Reader;
pub use transport::{CloseReport, Runner, ShellControl, ShellHandle};
