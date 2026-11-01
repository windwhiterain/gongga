//! Where a session's shells run: this machine, or a runner on another one.
//!
//! See `docs/notes/server-remote.md`.

use std::path::PathBuf;

use tokio::process::Command;

use crate::remote::transport::{Runner, ShellHandle};

/// The words of a runner command: `"gongga runner"` is two argv words on the peer.
///
/// See `docs/notes/server-remote.md`.
fn runner_words(runner: &str) -> Vec<&str> {
    runner.split_whitespace().collect()
}

/// A session's shells, and the machine they are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A shell started right here, exactly as this server has always done it.
    Local { program: PathBuf },
    /// A runner process that owns the shells, reached one of two ways.
    Remote {
        transport: Reach,
        /// The runner program, spelled for the peer.
        runner: String,
    },
    /// A runner on this machine, which is how a peer is exercised without ssh.
    Direct { runner: PathBuf },
}

/// How a runner process is started on the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// `ssh <host> gongga-runner`: host key checking stays `ssh`'s own.
    Ssh {
        host: String,
        port: Option<u16>,
        identity: Option<PathBuf>,
    },
}

impl Endpoint {
    /// The default endpoint: a shell on this machine.
    pub fn local(program: PathBuf) -> Self {
        Self::Local { program }
    }

    /// True when the shells are not this machine's.
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local { .. })
    }

    /// What a session is told it is running on.
    pub fn describe(&self) -> String {
        match self {
            Self::Local { program } => format!("local ({})", program.display()),
            Self::Direct { runner } => format!("direct runner ({})", runner.display()),
            Self::Remote {
                transport: Reach::Ssh { host, port, .. },
                runner,
            } => match port {
                Some(port) => format!("ssh {host}:{port} ({runner})"),
                None => format!("ssh {host} ({runner})"),
            },
        }
    }

    /// One shell on the endpoint, rooted at `cwd`.
    ///
    /// # Invariant
    ///
    /// A remote `cwd` is the endpoint's to accept: a directory it does not have is
    /// an error here, never a quiet substitution of this machine's.
    pub async fn connect(&self, cwd: Option<&str>) -> anyhow::Result<ShellHandle> {
        let command = match self {
            Self::Local { .. } => anyhow::bail!("the local endpoint has no runner"),
            Self::Direct { runner } => {
                let mut command = Command::new(runner);
                command.kill_on_drop(true);
                command
            }
            Self::Remote {
                transport:
                    Reach::Ssh {
                        host,
                        port,
                        identity,
                    },
                runner,
            } => {
                let mut command = Command::new("ssh");
                // Host key checking stays ssh's own; BatchMode only stops a hang.
                command.arg("-o").arg("BatchMode=yes");
                if let Some(port) = port {
                    command.arg("-p").arg(port.to_string());
                }
                if let Some(identity) = identity {
                    command.arg("-i").arg(identity);
                }
                // `ssh` joins what follows the host, so two words arrive as two.
                command.arg(host).arg("--");
                let words = runner_words(runner);
                if words.is_empty() {
                    anyhow::bail!("this target names an empty runner command");
                }
                command.args(&words);
                command.kill_on_drop(true);
                command
            }
        };
        let runner = Runner::spawn(command)
            .await
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.describe()))?;
        runner
            .open(cwd, None)
            .await
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.describe()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ssh command line must not carry anything that turns host checking off.
    #[test]
    fn an_ssh_endpoint_keeps_host_key_checking() {
        let endpoint = Endpoint::Remote {
            transport: Reach::Ssh {
                host: "peer".into(),
                port: Some(2222),
                identity: Some(PathBuf::from("key")),
            },
            runner: "gongga-runner".into(),
        };
        assert!(endpoint.is_remote());
        assert_eq!(endpoint.describe(), "ssh peer:2222 (gongga-runner)");
        assert!(!Endpoint::local(PathBuf::from("/bin/bash")).is_remote());
    }

    /// The default runner is the distribution's second word, not a program of its own.
    #[test]
    fn the_default_runner_is_one_the_distribution_dispatches() {
        assert_eq!(
            runner_words(crate::remote::runner::DEFAULT_RUNNER),
            vec!["gongga", "runner"]
        );
        assert_eq!(runner_words("  gongga runner "), vec!["gongga", "runner"]);
        assert_eq!(
            runner_words("/opt/gongga-runner"),
            vec!["/opt/gongga-runner"]
        );
        assert!(runner_words("   ").is_empty());
    }
}
