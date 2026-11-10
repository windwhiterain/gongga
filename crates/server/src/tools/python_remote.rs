//! A Python interpreter on the session's remote endpoint.
//!
//! See `docs/notes/server-tools-python.md`.

use std::path::Path;
use std::time::Duration;

use crate::remote::endpoint::Endpoint;
use crate::remote::protocol::{Event, Stream};
use crate::remote::transport::ShellHandle;
use crate::tools::file::Reply;

/// The names a peer's interpreter is started by, in the order tried.
///
/// # Invariant
///
/// Names, never a path: the program that runs on this machine is not evidence about
/// what that machine has, so the endpoint resolves these against its own `PATH`.
const PROGRAMS: [&str; 2] = ["python3", "python"];

/// What a candidate is asked before it is believed.
///
/// # Invariant
///
/// A name on the endpoint's `PATH` is not proof of an interpreter: on a Windows peer
/// `python3` is the Store's alias stub, which exits having run nothing at all. So the
/// first answer an interpreter gives is this one, and a name that cannot give it is
/// not that machine's Python.
const PROBE: &str = "1 + 1";

/// One interpreter on the endpoint, reached through the runner's own pipes.
pub struct RemoteRepl {
    /// Taken by `kill`, so a stopped interpreter cannot be written to again.
    handle: Option<ShellHandle>,
    /// Standard output the endpoint has sent and this side has not consumed.
    line: Vec<u8>,
    /// Everything the endpoint wrote to its stderr: a startup failure is read here.
    err: String,
}

impl RemoteRepl {
    /// Starts an interpreter on `endpoint`, standing in `cwd` as that machine spells it.
    ///
    /// # Invariant
    ///
    /// The directory is passed, never resolved here: a path this machine can open is
    /// not evidence about a path that machine has.
    pub async fn spawn(endpoint: &Endpoint, cwd: &Path, driver: &str) -> anyhow::Result<Self> {
        let at = cwd.to_string_lossy();
        let asked = (!at.trim().is_empty()).then_some(at.as_ref());
        let args = vec!["-u".to_string(), "-c".to_string(), driver.to_string()];
        let mut missing = Vec::new();
        for program in PROGRAMS {
            let Ok(handle) = endpoint
                .connect_program(asked, Some(program), Some(&args))
                .await
            else {
                // Not having that name is not a failure of the machine: try the next.
                missing.push(format!("{program}: it did not start"));
                continue;
            };
            let mut repl = Self {
                handle: Some(handle),
                line: Vec::new(),
                err: String::new(),
            };
            match repl.ask(&serde_json::json!({ "code": PROBE })).await {
                Ok(reply) if reply.ok => return Ok(repl),
                Ok(_) => {
                    repl.kill().await;
                    missing.push(format!("{program}: it answered nothing"));
                }
                Err(e) => {
                    repl.kill().await;
                    missing.push(format!("{program}: {e}"));
                }
            }
        }
        anyhow::bail!(
            "the endpoint has no Python to start: {}. Install one on that machine, or run \
             this on the machine that has it",
            missing.join("; ")
        )
    }
    /// Sends one request and waits for its answer line, until `timeout` passes.
    pub async fn run_request(
        &mut self,
        request: &serde_json::Value,
        timeout: Duration,
    ) -> anyhow::Result<Reply> {
        self.send(request).await?;
        match tokio::time::timeout(timeout, self.reply()).await {
            Err(_) => anyhow::bail!(
                "the Python call did not finish within {}s",
                timeout.as_secs()
            ),
            Ok(reply) => reply,
        }
    }

    /// How this interpreter is stopped without taking its lock.

    /// One request and its answer, with no clock.
    ///
    /// # Invariant
    ///
    /// The answer is an event: an interpreter that ended says so instead of waiting
    /// to be timed out.
    async fn ask(&mut self, request: &serde_json::Value) -> anyhow::Result<Reply> {
        self.send(request).await?;
        self.reply().await
    }

    /// Writes one request line to the interpreter.
    async fn send(&mut self, request: &serde_json::Value) -> anyhow::Result<()> {
        let Some(handle) = &self.handle else {
            anyhow::bail!("this interpreter on the endpoint was stopped");
        };
        let mut line = request.to_string();
        line.push('\n');
        handle.write(line.as_bytes()).await
    }

    /// One whole line from the interpreter, or why there will not be one.
    async fn reply(&mut self) -> anyhow::Result<Reply> {
        loop {
            if let Some(reply) = self.take_line()? {
                return Ok(reply);
            }
            if self.step().await? {
                // A process that ended cannot answer; say what it said instead.
                anyhow::bail!(self.why_ended());
            }
        }
    }

    /// Takes what the endpoint said next; `true` when it can say nothing more.
    async fn step(&mut self) -> anyhow::Result<bool> {
        let Some(handle) = &mut self.handle else {
            anyhow::bail!("this interpreter on the endpoint was stopped");
        };
        match handle.next_event().await {
            Some(Event::Out { stream, data, .. }) => {
                let Some(bytes) = crate::base64::decode(&data) else {
                    anyhow::bail!("the endpoint sent output this build cannot read");
                };
                match stream {
                    Stream::Stdout => self.line.extend_from_slice(&bytes),
                    Stream::Stderr => self.err.push_str(&String::from_utf8_lossy(&bytes)),
                }
                Ok(false)
            }
            Some(Event::End { .. }) => Ok(true),
            Some(Event::Hello { .. }) => Ok(false),
            None => {
                self.err
                    .push_str("\nthe endpoint's connection ended while waiting for Python");
                Ok(true)
            }
        }
    }

    /// The next whole reply line, when the endpoint has already sent one.
    fn take_line(&mut self) -> anyhow::Result<Option<Reply>> {
        loop {
            let Some(at) = self.line.iter().position(|byte| *byte == b'\n') else {
                return Ok(None);
            };
            let mut line: Vec<u8> = self.line.drain(..=at).collect();
            line.pop();
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if text.is_empty() {
                continue;
            }
            let reply = serde_json::from_str::<Reply>(&text)
                .map_err(|e| anyhow::anyhow!("a malformed Python reply: {e}"))?;
            return Ok(Some(reply));
        }
    }

    /// Why an interpreter on the endpoint stopped, in the words its machine used.
    fn why_ended(&self) -> String {
        let said = self.err.trim();
        if said.is_empty() {
            "the Python interpreter on the endpoint ended".to_string()
        } else {
            format!("the Python interpreter on the endpoint ended: {said}")
        }
    }

    /// Ends the interpreter from inside its own lock, when the caller holds it.
    pub async fn kill(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.close().await;
        }
    }
}
