//! One runner, driven the way a remote session drives it.
//!
//! See `docs/notes/server-remote.md`.

use std::path::Path;
use std::time::Duration;

use gongga_server::remote::protocol::{Event, Op, Stream};
use gongga_server::remote::transport::{Runner, ShellHandle};
use tokio::process::Command;

/// Long enough that only the thing named can have answered inside it.
const PATIENCE: Duration = Duration::from_secs(20);

fn runner() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gongga-runner"));
    command.kill_on_drop(true);
    command
}

/// Collects a shell's output until `needle` appears, returning what was read.
async fn until(shell: &mut ShellHandle, needle: &str) -> String {
    let mut seen = String::new();
    loop {
        let event = tokio::time::timeout(PATIENCE, shell.next_event())
            .await
            .expect("the endpoint said nothing")
            .expect("the shell's channel ended");
        if let Some(bytes) = event.bytes() {
            seen.push_str(&String::from_utf8_lossy(&bytes));
            if seen.contains(needle) {
                return seen;
            }
        }
    }
}

/// Closes a shell, bounded: an endpoint that never answers must fail, not hang.
async fn closed_within(shell: ShellHandle) -> gongga_server::remote::transport::CloseReport {
    tokio::time::timeout(PATIENCE, shell.close())
        .await
        .expect("the endpoint never answered the close")
}

/// The end of a shell, with the status the endpoint reported.
async fn ended(shell: &mut ShellHandle) -> Option<i32> {
    loop {
        let event = tokio::time::timeout(PATIENCE, shell.next_event())
            .await
            .expect("the shell never ended")
            .expect("the shell's channel ended");
        if let Event::End { code, .. } = event {
            return code;
        }
    }
}

/// The size of `path` right now, or 0 before it exists.
fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// A command leaving a child behind, ticking a file so its death is observable.
fn ticking_child(file: &Path) -> String {
    format!(
        "( while : ; do printf x >> '{file}'; sleep 0.1; done ) &\n",
        file = file.to_string_lossy()
    )
}

/// Waits until the file stops growing, giving up after `within`.
async fn stops_ticking(file: &Path, within: Duration) -> bool {
    let mut last = size(file);
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let now = size(file);
        if now == last {
            return now > 0;
        }
        last = now;
    }
    false
}

/// A signal and a file read both answer while one shell waits inside `read`.
///
/// # Invariant
///
/// The answers cannot have come from the command finishing: the shell is still
/// blocked in its builtin when the signal is sent.
#[tokio::test]
async fn a_signal_and_a_read_answer_while_a_command_holds_the_shell() {
    let dir = tempfile::tempdir().expect("a directory the endpoint owns");
    let file = dir.path().join("payload.txt");
    std::fs::write(&file, b"endpoint bytes\n").expect("writing the endpoint's file");
    let path = file.to_string_lossy().into_owned();

    let runner = Runner::spawn(runner()).await.expect("a runner");
    assert!(runner.pid() != 0, "the greeting carries the peer's pid");
    let mut shell = runner.open(None, None).await.expect("a shell");

    // A builtin blocks the shell without starting a process that could outlive it.
    shell
        .write(b"printf 'ready\\n'; read -r line\n")
        .await
        .expect("writing a command");
    assert_eq!(until(&mut shell, "ready").await.trim(), "ready");

    // Both of these are answered while that command still holds the shell.
    let what = runner.stat(&path).await.expect("a stat answer");
    assert!(what.exists && what.is_file, "{what:?}");
    let chunk = runner.read(&path, 0).await.expect("a read answer");
    assert_eq!(chunk.data, b"endpoint bytes\n");
    assert!(chunk.eof);

    assert!(
        shell.signal("TERM").await.expect("a signal answer"),
        "the endpoint did not deliver the signal"
    );
    let code = ended(&mut shell).await;
    assert!(
        code.is_some_and(|c| c != 0),
        "a signalled shell must not report success: {code:?}"
    );
    assert!(
        runner.connected(),
        "answering once must not end the transport"
    );
}

/// Closing a shell ends its whole group, and says only what it proved.
///
/// # Invariant
///
/// The named child stops ticking: a `close` that left it running would be a
/// confirmed deletion of nothing.
#[tokio::test]
async fn a_closed_shell_leaves_nothing_of_it_running() {
    let dir = tempfile::tempdir().expect("a directory the endpoint owns");
    let marks = dir.path().join("ticks.txt");
    let runner = Runner::spawn(runner()).await.expect("a runner");
    let shell = runner.open(None, None).await.expect("a shell");
    let id = shell.id();

    shell
        .write(ticking_child(&marks).as_bytes())
        .await
        .expect("starting the child");
    assert!(
        tokio::time::timeout(PATIENCE, async {
            while size(&marks) == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .is_ok(),
        "the child never started ticking"
    );

    // Output nobody has read yet: a close must not wait behind it.
    shell
        .write(b"head -c 2000000 /dev/zero | tr '\\0' x\n")
        .await
        .expect("writing more output");
    let report = closed_within(shell).await;
    assert!(
        report.confirmed,
        "the endpoint did not confirm the group is gone: {:?}",
        report.unknown
    );
    assert!(
        stops_ticking(&marks, Duration::from_secs(5)).await,
        "the shell's child outlived a confirmed close"
    );
    // Nothing is left behind the name: a late write or signal is refused, never
    // aimed at whatever holds that id now.
    let late = runner
        .request(Op::Signal {
            shell: id,
            signal: "TERM".to_string(),
        })
        .await;
    let message = late.expect_err("a closed shell must refuse a signal");
    assert!(message.to_string().contains("no shell"), "{message}");
}

/// A dropped connection is not a silent abandonment: the endpoint ends its shells.
#[tokio::test]
async fn dropping_the_connection_ends_the_endpoints_shells() {
    let dir = tempfile::tempdir().expect("a directory the endpoint owns");
    let marks = dir.path().join("ticks.txt");
    let runner = Runner::spawn(runner()).await.expect("a runner");
    let shell = runner.open(None, None).await.expect("a shell");
    shell
        .write(ticking_child(&marks).as_bytes())
        .await
        .expect("starting the child");
    assert!(
        tokio::time::timeout(PATIENCE, async {
            while size(&marks) == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .is_ok(),
        "the child never started ticking"
    );

    drop(shell);
    drop(runner);
    assert!(
        stops_ticking(&marks, Duration::from_secs(15)).await,
        "a dropped connection left a process running on the endpoint"
    );
}

/// Every byte, then the end: a shell's tail is not lost behind its `End`.
#[tokio::test]
async fn the_two_streams_arrive_separately_and_the_status_is_exact() {
    let runner = Runner::spawn(runner()).await.expect("a runner");
    let mut shell = runner.open(None, None).await.expect("a shell");
    // Read slowly, so both pipes still hold bytes at exit: `End` must follow them.
    let bytes = 2_000_000;
    let command = format!(
        "printf 'to-stdout\\n'; head -c {bytes} /dev/zero | tr '\\0' o; \
         printf 'to-stderr\\n' >&2; head -c {bytes} /dev/zero | tr '\\0' e >&2; exit 7\n"
    );
    shell
        .write(command.as_bytes())
        .await
        .expect("writing a command");

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut seqs = Vec::new();
    let mut code = None;
    while code.is_none() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let event = tokio::time::timeout(PATIENCE, shell.next_event())
            .await
            .expect("the endpoint said nothing")
            .expect("the shell's channel ended");
        match &event {
            Event::Out { stream, seq, .. } => {
                let text =
                    String::from_utf8_lossy(&event.bytes().expect("base64 bytes")).into_owned();
                match stream {
                    Stream::Stdout => {
                        seqs.push(*seq);
                        stdout.push_str(&text);
                    }
                    Stream::Stderr => stderr.push_str(&text),
                }
            }
            Event::End { code: exited, .. } => {
                code = Some(*exited);
                break;
            }
            Event::Hello { .. } => {}
        }
    }
    assert!(stdout.starts_with("to-stdout\n"), "{:?}", &stdout[..40]);
    assert!(
        stdout.len() == "to-stdout\n".len() + bytes,
        "stdout lost bytes: {} of {}",
        stdout.len() - "to-stdout\n".len(),
        bytes
    );
    assert!(
        stderr.len() == "to-stderr\n".len() + bytes,
        "stderr lost bytes: {} of {}",
        stderr.len() - "to-stderr\n".len(),
        bytes
    );
    assert_eq!(
        seqs,
        (0..seqs.len() as u64).collect::<Vec<_>>(),
        "each stream numbers its own chunks from zero, without a gap"
    );
    assert_eq!(code, Some(Some(7)), "the status came from the shell itself");
    // Nothing may follow the end: a client stops reading there, so a byte emitted
    // after it is a byte lost.
    if let Ok(Some(after)) =
        tokio::time::timeout(Duration::from_millis(300), shell.next_event()).await
    {
        panic!("the endpoint said {after:?} after the shell ended");
    }
}

/// A runner whose client stopped reading is not left running blind (unix only).
#[cfg(unix)]
#[tokio::test]
async fn a_runner_that_cannot_write_is_not_left_running() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut command = Command::new(env!("CARGO_BIN_EXE_gongga-runner"));
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("a runner");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");

    // The greeting, read byte by byte so nothing past it is consumed.
    let mut greeting = Vec::new();
    while !greeting.ends_with(b"\n") {
        let mut byte = [0u8; 1];
        let read = tokio::time::timeout(PATIENCE, stdout.read(&mut byte))
            .await
            .expect("the runner said nothing")
            .expect("reading the greeting");
        assert!(read == 1, "the runner closed before greeting");
        greeting.push(byte[0]);
    }
    assert!(
        String::from_utf8_lossy(&greeting).contains("hello"),
        "{:?}",
        String::from_utf8_lossy(&greeting)
    );
    // Nothing will ever read what it writes again.
    drop(stdout);

    let _ = stdin
        .write_all(b"{\"id\":1,\"op\":\"open\",\"shell\":1}\n")
        .await;
    let _ = stdin.flush().await;
    let ended = tokio::time::timeout(PATIENCE, child.wait()).await;
    // However this ends, the process is not left behind.
    let _ = child.start_kill();
    let _ = child.wait().await;
    assert!(
        ended.is_ok(),
        "the runner kept running with no way to report anything"
    );
}

/// A session's shell on a peer: `cd`, an export and an exact status survive,
/// through the same types the session holds.
#[tokio::test]
async fn an_endpoint_shell_keeps_its_state_across_calls() {
    let root = tempfile::tempdir().expect("a directory the endpoint owns");
    let runner = env!("CARGO_BIN_EXE_gongga-runner").to_string();
    let mut shells = gongga_server::remote::ShellSet::new(
        gongga_server::remote::Endpoint::Direct {
            runner: runner.into(),
        },
        Some(root.path().to_path_buf()),
        Vec::new(),
    );
    let handle = shells.get("default").expect("the default shell");
    let mut shell = handle.lock().await;
    assert!(shell.is_remote(), "the shell must be the endpoint's");

    let first = shell
        .run(
            "cd / && export GONGGA_PROBE=kept",
            Duration::from_secs(20),
            None,
        )
        .await
        .expect("a command");
    assert_eq!(first.exit_code, Some(0), "{:?}", first.stderr);
    let second = shell
        .run(
            "pwd; printf '%s' \"$GONGGA_PROBE\"",
            Duration::from_secs(20),
            None,
        )
        .await
        .expect("a second command");
    assert_eq!(
        second.stdout.lines().next(),
        Some("/"),
        "{:?}",
        second.stdout
    );
    assert!(second.stdout.contains("kept"), "{:?}", second.stdout);

    // The status is the endpoint's own: the subshell fails without taking the shell.
    let failed = shell
        .run("(exit 42)", Duration::from_secs(20), None)
        .await
        .expect("a failing command");
    assert_eq!(failed.exit_code, Some(42));

    // Background is refused here rather than run on this machine.
    let refused = shell.run_background("echo hi", None).await;
    let message = match refused {
        Ok(_) => panic!("a remote background job must be refused"),
        Err(e) => e.to_string(),
    };
    assert!(message.contains("remote endpoint"), "{message}");

    // Teardown proves what it ended.
    assert!(shell.pid() != 0, "the endpoint reported no pid");
    let report = shell.kill().await;
    assert!(report.confirmed, "{:?}", report.unknown);

    // No sentinel means no status invented, and no signal through a reaped pid.
    let gone = shells.get("ended").expect("a second shell");
    let mut gone = gone.lock().await;
    let ended = gone
        .run("exit 7", Duration::from_secs(20), None)
        .await
        .expect("a command that ends the shell");
    assert_eq!(ended.exit_code, None);
    assert!(!gone.usable(), "an ended shell must not be reusable");
    let report = gone.kill().await;
    assert!(!report.confirmed, "an ended shell cannot be proved gone");
    assert!(
        report.unknown.unwrap_or_default().contains("already ended"),
        "the report must say why it is unknown"
    );
}

/// A directory this machine does not have is refused, never replaced by one it does.
#[tokio::test]
async fn an_unknown_directory_is_refused_by_the_endpoint() {
    let runner = Runner::spawn(runner()).await.expect("a runner");
    let refused = runner
        .open(Some("/definitely/not/a/real/directory"), None)
        .await;
    let message = match refused {
        Ok(_) => panic!("a directory that is not there must not become a shell"),
        Err(e) => e.to_string(),
    };
    assert!(
        message.contains("/definitely/not/a/real/directory") || message.contains("directory"),
        "{message}"
    );
    assert!(runner.connected(), "a refusal is not a lost connection");
}
