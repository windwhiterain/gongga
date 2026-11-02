//! One machine's shells, driven over a stdio runner process.
//!
//! See `docs/notes/server-remote.md`.

pub mod endpoint;
pub mod files;
pub mod protocol;
pub mod runner;
pub mod shell;
pub mod shell_set;
pub mod transport;

pub use endpoint::{Endpoint, Reach};
pub use files::Reader;
pub use shell_set::{Shell, ShellSet};
pub use transport::{CloseReport, Runner, ShellControl, ShellHandle};
