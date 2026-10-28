//! The agent's own door into the harness: commands, without the CLI.
//!
//! See `docs/notes/server-tools-harness.md`.
//!
//! # Invariant
//!
//! Calls `dispatch` in-process. The agent lives in this server, so spawning a
//! CLI to dial the same server over UDP would be a loop through the network for
//! something one function call away -- and it would make the agent depend on a
//! binary being installed and reachable.

use std::sync::Weak;

use serde_json::{Value, json};

use crate::commands::{Dispatch, delete_own_session, dispatch};
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// Runs one harness command, or lists the ones that exist.
pub struct HarnessTool {
    /// Empty only while the dispatch that owns this tool is being built.
    dispatch: Weak<Dispatch>,
    /// Longer replies spill to a file: in-process there is no channel limit.
    max_output_chars: usize,
}

impl HarnessTool {
    pub fn new(dispatch: Weak<Dispatch>, max_output_chars: usize) -> Self {
        Self {
            dispatch,
            max_output_chars,
        }
    }

    /// The catalog as the model reads it: the published list, not a copy.
    async fn listing(d: &Dispatch) -> String {
        let catalog = d.catalog().await;
        let mut lines = vec![format!(
            "{} command(s); call this tool with `command` and `arguments`.",
            catalog.len()
        )];
        for c in catalog {
            // A client command is the operator's, not the agent's: say so once
            // rather than pretending it is missing.
            let target = match c.target {
                protocol::CommandTarget::Server => String::new(),
                protocol::CommandTarget::ServerScript => " [script]".to_string(),
                protocol::CommandTarget::Client => " [client-side only]".to_string(),
            };
            lines.push(format!(
                "{}{} -- {}\n    takes: {}",
                c.name,
                target,
                c.summary,
                signature(&c.args),
            ));
        }
        lines.join("\n")
    }
}

/// `--a <a: required>`, or `nothing` for a command with no arguments.
fn signature(args: &[protocol::ArgSpec]) -> String {
    if args.is_empty() {
        return "nothing".to_string();
    }
    args.iter()
        .map(|a| {
            let needed = if a.required { "required" } else { "optional" };
            format!("--{} ({needed})", a.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The tool's arguments as the protocol's string map.
///
/// # Invariant
///
/// One string per name, whatever JSON was passed: `Args` is what every command
/// already takes, and a number or a nested object becomes its JSON spelling
/// rather than being rejected for its type.
fn as_args(raw: Option<&Value>) -> Result<protocol::Args, String> {
    let mut args = protocol::Args::new();
    let Some(value) = raw else {
        return Ok(args);
    };
    let Some(map) = value.as_object() else {
        return Err(format!(
            "`arguments` must be an object of argument names, and this was {value}"
        ));
    };
    for (name, value) in map {
        if value.is_null() {
            continue;
        }
        let text = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        args.insert(name.clone(), text);
    }
    Ok(args)
}

impl Tool for HarnessTool {
    fn name(&self) -> &'static str {
        "harness"
    }

    fn description(&self) -> &'static str {
        "Run a gongga harness command in this server. Without `command`, lists what exists. \
         This is the same command surface the CLI has, reached in-process: `cancel`, \
         `set_session`, `send_message`, `interrupt_session`, `del_session`, `get_messages`, \
         and anything a script registered."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Command to run, without a leading `/`, e.g. `cancel`. \
                                    Omit to list the commands and their arguments."
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments by name, without leading dashes, e.g. \
                                    {\"session\": \"main\"}. Values may be strings, numbers or \
                                    booleans."
                }
            },
            "required": []
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(d) = self.dispatch.upgrade() else {
                return ToolOutcome::err("the harness is not available in this server");
            };
            let command = args.get("command").and_then(Value::as_str).unwrap_or("");

            if command.trim().is_empty() {
                return ToolOutcome::ok(Self::listing(&d).await);
            }
            let command = command.trim().trim_start_matches('/');
            let named = match as_args(args.get("arguments")) {
                Ok(a) => a,
                Err(e) => return ToolOutcome::err(e),
            };

            let outcome = if command == "del_session" {
                match delete_own_session(&d, &named, &ctx.session) {
                    Some(outcome) => outcome,
                    None => dispatch(&d, command, &named).await,
                }
            } else {
                dispatch(&d, command, &named).await
            };
            let mut text = outcome.message;
            // The data is the answer for a read; the message alone is a summary.
            if let Some(data) = outcome.data {
                text.push('\n');
                text.push_str(&serde_json::to_string_pretty(&data).unwrap_or_default());
            }
            let capped = crate::tools::bash::cap_output(
                crate::prompts::shared(),
                &text,
                self.max_output_chars,
                &ctx.output_dir,
                &format!("{}-harness-{}", ctx.session, ctx.call_id),
            );
            if outcome.ok {
                ToolOutcome::ok(capped)
            } else {
                ToolOutcome::err(capped)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;
    use protocol::ArgSpec;
    use std::sync::Arc;

    /// A whole server minus what listens: no socket, no CLI, no subprocess.
    fn dispatch_in(dir: &std::path::Path) -> Arc<Dispatch> {
        use crate::config::{BashConfig, LlmConfig};
        use crate::session::SessionManager;
        use crate::tools::detached::DetachedRegistry;
        use std::sync::atomic::AtomicUsize;

        let store = crate::store::Store::open(&dir.join("g.db")).unwrap();
        let llm = Arc::new(
            crate::llm::LlmClient::new(&LlmConfig {
                provider: None,
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "m".into(),
                context: None,
                api_key_env: String::new(),
                api_key: "k".into(),
                temperature: None,
                max_output_tokens: None,
                store: false,
                request_timeout: std::time::Duration::from_secs(1),
                idle_timeout: std::time::Duration::from_secs(1),
                max_retries: 0,
                retry_base: std::time::Duration::from_millis(1),
                builtin_tools: vec![],
                modalities: vec![],
            })
            .unwrap(),
        );
        let bash = BashConfig {
            // `set_session` materialises a session, which spawns its shell; that
            // is the session's dependency, not this tool's.
            program: crate::config::detect_git_bash(),
            timeout: std::time::Duration::from_secs(1),
            max_output_chars: 0,
        };
        let detached = DetachedRegistry::new(store.clone(), None, dir.join("detached"), vec![]);
        let sessions =
            SessionManager::new(store.clone(), bash, "m".into(), detached.clone(), vec![]);
        let bus = crate::bus::EventBus::new(16);
        let scripts = crate::script::Scripts::new(dir.join("scripts"), None, vec![]);
        let out_dir = dir.to_path_buf();
        Arc::new_cyclic(|weak| {
            let tools = Arc::new(
                ToolRegistry::with_builtins(std::time::Duration::from_secs(1), 0)
                    .with_harness(weak.clone(), 0),
            );
            let agent = Arc::new(crate::agent::Agent {
                store: store.clone(),
                llm,
                providers: crate::providers::Handle,
                llm_config: LlmConfig {
                    provider: None,
                    base_url: "http://127.0.0.1:1/v1".into(),
                    model: "m".into(),
                    context: None,
                    api_key_env: String::new(),
                    api_key: "k".into(),
                    temperature: None,
                    max_output_tokens: None,
                    store: false,
                    request_timeout: std::time::Duration::from_secs(1),
                    idle_timeout: std::time::Duration::from_secs(1),
                    max_retries: 0,
                    retry_base: std::time::Duration::from_millis(1),
                    builtin_tools: vec![],
                    modalities: vec![],
                },
                clients: std::sync::Mutex::new(std::collections::HashMap::new()),
                default_context: None,
                tools,
                bus: bus.clone(),
                compress_above_tokens: 0,
                output_dir: out_dir,
                detached,
            });
            Dispatch {
                agent,
                sessions,
                store,
                bus,
                scripts,
                providers: crate::providers::Handle,
                active: Arc::new(AtomicUsize::new(0)),
                shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                shutdown: tokio_util::sync::CancellationToken::new(),
            }
        })
    }

    /// A context with nothing behind it: the harness tool needs no shell.
    fn ctx(dir: &std::path::Path) -> ToolContext {
        use crate::session::{InboxSignal, ShellSet};
        use std::sync::atomic::AtomicU64;
        ToolContext {
            session: "agent".into(),
            call_id: "call-1".into(),
            output_dir: dir.to_path_buf(),
            shells: Arc::new(std::sync::Mutex::new(ShellSet::new(
                std::path::PathBuf::from("bash"),
                dir.to_path_buf(),
                vec![],
            ))),
            detached: crate::tools::detached::DetachedRegistry::new(
                crate::store::Store::open(&dir.join("g.db")).unwrap(),
                None,
                dir.join("detached"),
                vec![],
            ),
            events: None,
            cancel: tokio_util::sync::CancellationToken::new(),
            store: crate::store::Store::open(&dir.join("g.db")).unwrap(),
            cwd: dir.to_path_buf(),
            vision: crate::llm::Vision {
                accepts_image: false,
                refusal: String::new(),
            },
            inbox: InboxSignal::around(Arc::new(AtomicU64::new(0))),
        }
    }

    fn tool(d: &Arc<Dispatch>) -> HarnessTool {
        HarnessTool::new(Arc::downgrade(d), 0)
    }

    /// No `command` lists the catalog, client commands included.
    #[tokio::test]
    async fn an_empty_call_lists_every_command() {
        let dir = tempfile::tempdir().unwrap();
        let d = dispatch_in(dir.path());
        let out = tool(&d).run(json!({}), &ctx(dir.path())).await;
        assert!(out.ok, "{}", out.output);
        for expected in ["send_message", "get_messages", "cancel", "set_session"] {
            assert!(
                out.output.contains(expected),
                "missing {expected}: {}",
                out.output
            );
        }
        assert!(
            out.output.contains("[client-side only]"),
            "the client's commands are named as such: {}",
            out.output
        );
    }

    /// A real command runs with no server listening and no CLI on the machine.
    #[tokio::test]
    async fn a_command_runs_in_process_and_its_data_comes_back() {
        let Some(_) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let d = dispatch_in(dir.path());
        let c = ctx(dir.path());

        let made = tool(&d)
            .run(
                json!({"command": "set_session", "arguments": {"session": "inproc"}}),
                &c,
            )
            .await;
        assert!(made.ok, "{}", made.output);

        let read = tool(&d)
            .run(
                json!({"command": "get_messages",
                       "arguments": {"session": "inproc", "range": "-5:"}}),
                &c,
            )
            .await;
        assert!(read.ok, "{}", read.output);
        // The reply carries the data, not only a sentence about it.
        assert!(read.output.contains("\"last_seq\""), "{}", read.output);
        assert!(read.output.contains("\"status\""), "{}", read.output);
        assert!(read.output.contains("inproc"), "{}", read.output);

        // And it really was in this process's store.
        let names: Vec<String> = d
            .store
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["inproc"]);
    }

    /// A leading `/` is the input line's syntax, and is accepted anyway.
    #[tokio::test]
    async fn a_leading_slash_is_not_a_mistake_here() {
        let Some(_) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let d = dispatch_in(dir.path());
        let out = tool(&d)
            .run(
                json!({"command": "/set_session", "arguments": {"session": "slashed"}}),
                &ctx(dir.path()),
            )
            .await;
        assert!(out.ok, "{}", out.output);
        assert!(d.store.list_sessions().unwrap().len() == 1);
    }

    /// Nothing installed, nothing listening: `cancel` needs only store and bus.
    #[tokio::test]
    async fn a_command_needing_nothing_answers_in_process() {
        let dir = tempfile::tempdir().unwrap();
        let d = dispatch_in(dir.path());
        let out = tool(&d)
            .run(json!({"command": "cancel"}), &ctx(dir.path()))
            .await;
        assert!(out.ok, "{}", out.output);
        assert!(
            out.output.contains("cancelling 0 session(s)"),
            "{}",
            out.output
        );
        // It stops *this* dispatch -- the one the tool reached in-process.
        assert_eq!(d.active.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A client command is refused by the harness itself: the agent must not
    /// write the operator's own client state.
    #[tokio::test]
    async fn a_client_command_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let d = dispatch_in(dir.path());
        let out = tool(&d)
            .run(
                json!({"command": "set_sticky_cmd_argument",
                       "arguments": {"name": "session", "value": "x"}}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!out.ok, "{}", out.output);
        assert!(out.output.contains("runs in the client"), "{}", out.output);
    }

    /// A missing argument comes back as the command's own refusal, not a panic.
    #[tokio::test]
    async fn a_missing_argument_is_the_command_s_own_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let d = dispatch_in(dir.path());
        let out = tool(&d)
            .run(json!({"command": "get_messages"}), &ctx(dir.path()))
            .await;
        assert!(!out.ok);
        assert!(out.output.contains("missing required"), "{}", out.output);
    }

    #[test]
    fn a_signature_names_every_argument_and_whether_it_is_required() {
        let args = vec![
            ArgSpec {
                name: "session".into(),
                summary: String::new(),
                required: true,
                value_hint: None,
                completions: Default::default(),
            },
            ArgSpec {
                name: "cwd".into(),
                summary: String::new(),
                required: false,
                value_hint: None,
                completions: Default::default(),
            },
        ];
        assert_eq!(signature(&args), "--session (required), --cwd (optional)");
        assert_eq!(signature(&[]), "nothing");
    }

    /// Whatever JSON was passed becomes the string map the commands take; a
    /// null is "not given" rather than the text "null".
    #[test]
    fn arguments_become_the_string_map_a_command_takes() {
        let raw = json!({
            "session": "main",
            "compact_threshold": 30,
            "thinking_effort": null,
            "nested": {"a": 1}
        });
        let args = as_args(Some(&raw)).unwrap();
        assert_eq!(args["session"], "main");
        assert_eq!(args["compact_threshold"], "30");
        assert!(!args.contains_key("thinking_effort"), "null means absent");
        assert_eq!(args["nested"], r#"{"a":1}"#);

        assert!(as_args(None).unwrap().is_empty());
        // A list is not a set of arguments, and saying which shape was wanted
        // is more use than a type error.
        assert!(as_args(Some(&json!(["a"]))).unwrap_err().contains("object"));
    }
}
