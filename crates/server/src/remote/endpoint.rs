//! Where a session's shells run: this machine, or a runner on another one.
//!
//! See `docs/notes/server-remote.md`.

use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

use crate::remote::transport::{ProtocolMismatch, Runner, ShellHandle};

/// One argument of a command line, quoted when it carries a space.
///
/// See `docs/notes/server-remote.md`.
fn quoted(value: &str) -> String {
    if value.contains(char::is_whitespace) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

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
        /// The longest this peer may stay silent, at its greeting and at every answer.
        ///
        /// # Invariant
        ///
        /// The network clock, and the reason it is carried here: it belongs to the
        /// link, not to a tool. What it ends is a request, with an error; the clock
        /// a call goes to the background on is the caller's own, measured locally.
        /// See `docs/notes/server-remote.md`.
        answer: Duration,
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

    /// How long this endpoint's peer may stay silent before an answer is an error.
    ///
    /// # Invariant
    ///
    /// Never unbounded, even on a runner this machine starts itself: a silence that
    /// no clock ends is a call that never returns.
    pub fn answer_timeout(&self) -> Duration {
        match self {
            Self::Remote { answer, .. } => *answer,
            Self::Local { .. } | Self::Direct { .. } => {
                crate::remote::transport::DEFAULT_ANSWER_TIMEOUT
            }
        }
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
                ..
            } => match port {
                Some(port) => format!("ssh {host}:{port} ({runner})"),
                None => format!("ssh {host} ({runner})"),
            },
        }
    }

    /// The error, with the way out of it when there is one.
    ///
    /// # Invariant
    ///
    /// A peer that speaks another protocol is the one failure a reader can act
    /// on without reading the source: the distribution on the peer has to be
    /// replaced and this machine's remote pointed at the new one. The refusal
    /// carries that command, because a refusal naming only two numbers leaves
    /// the reader to work the repair out alone.
    fn explain(&self, error: anyhow::Error) -> anyhow::Error {
        match error.downcast::<ProtocolMismatch>() {
            Ok(mismatch) => match self.remedy() {
                Some(remedy) => anyhow::anyhow!("{}: {mismatch}; {remedy}", self.describe()),
                None => anyhow::anyhow!("{}: {mismatch}", self.describe()),
            },
            Err(error) => anyhow::anyhow!("{}: {error}", self.describe()),
        }
    }

    /// The command that replaces the distribution on the peer.
    ///
    /// # Invariant
    ///
    /// Only an endpoint reached over ssh has a peer to reinstall, and the
    /// command is the deployment tool's own: one a reader can run as it stands.
    fn remedy(&self) -> Option<String> {
        let Endpoint::Remote {
            transport:
                Reach::Ssh {
                    host,
                    port,
                    identity,
                },
            ..
        } = self
        else {
            return None;
        };
        let (user, address) = match host.rsplit_once('@') {
            Some((user, address)) => (Some(user), address),
            None => (None, host.as_str()),
        };
        let mut command = String::from("gongga-cli skill remote install --host ");
        command.push_str(address);
        if let Some(user) = user {
            command.push_str(" --user ");
            command.push_str(user);
        }
        if let Some(port) = port {
            command.push_str(" --port ");
            command.push_str(&port.to_string());
        }
        if let Some(identity) = identity {
            command.push_str(" --identity ");
            command.push_str(&quoted(&identity.display().to_string()));
        }
        Some(format!(
            "reinstall the distribution on the peer with `{command}`, point this machine's \
             remote at the runner it prints, and restart the server"
        ))
    }

    /// The command that starts a runner for this endpoint.
    ///
    /// # Invariant
    ///
    /// One spelling of it: a shell and a file read reach the same peer the same way,
    /// so a target that works for one works for the other. See
    /// `docs/notes/server-remote.md`.
    pub fn runner_command(&self) -> anyhow::Result<Command> {
        match self {
            Self::Local { .. } => anyhow::bail!("the local endpoint has no runner"),
            Self::Direct { runner } => {
                let mut command = Command::new(runner);
                command.kill_on_drop(true);
                Ok(command)
            }
            Self::Remote {
                transport:
                    Reach::Ssh {
                        host,
                        port,
                        identity,
                    },
                runner,
                ..
            } => {
                let mut command = Command::new("ssh");
                // Host key checking stays ssh's own; BatchMode only stops a hang.
                command.arg("-o").arg("BatchMode=yes");
                // An idle connection must be found dead: three unanswered
                // probes a quarter minute apart end it.
                command
                    .arg("-o")
                    .arg("ServerAliveInterval=15")
                    .arg("-o")
                    .arg("ServerAliveCountMax=3")
                    .arg("-o")
                    .arg("TCPKeepAlive=yes");
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
                Ok(command)
            }
        }
    }

    /// One shell on the endpoint, rooted at `cwd`.
    ///
    /// # Invariant
    ///
    /// A remote `cwd` is the endpoint's to accept: a directory it does not have is
    /// an error here, never a quiet substitution of this machine's.
    pub async fn connect(&self, cwd: Option<&str>) -> anyhow::Result<ShellHandle> {
        self.connect_program(cwd, None, None).await
    }

    /// One program on the endpoint, with the arguments it is to be started with.
    ///
    /// # Invariant
    ///
    /// The program is named for that machine, never resolved here: a path this
    /// machine holds is not evidence about one the peer has.
    pub async fn connect_program(
        &self,
        cwd: Option<&str>,
        program: Option<&str>,
        args: Option<&[String]>,
    ) -> anyhow::Result<ShellHandle> {
        let command = self.runner_command()?;
        let runner = Runner::spawn(command, self.answer_timeout())
            .await
            .map_err(|e| self.explain(e))?;
        runner
            .open_with(cwd, program, args)
            .await
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.describe()))
    }
}
