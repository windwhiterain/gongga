//! The agent-written checkpoint artifact; the agent loop commits it with its result.

use serde_json::Value;

use crate::tools::{Cmd, ToolContext, ToolOutcome};

pub const LIVE_COMPACT_TOOL: &str = "live_compact";

pub struct LiveCompactTool;

impl Cmd for LiveCompactTool {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let settled = args
                .get("settled")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if settled.is_empty() {
                return ToolOutcome::err("settled must not be empty");
            }
            // Where this session's references are read: the machine it stands on.
            let source = match (&ctx.reader, ctx.machine) {
                (Some(reader), crate::session::Machine::Remote) => {
                    crate::meta::Source::Remote(reader)
                }
                (None, crate::session::Machine::Remote) => crate::meta::Source::Unreachable,
                (_, crate::session::Machine::Local) => crate::meta::Source::Local,
            };
            ToolOutcome::ok(crate::meta::embed(source, &ctx.cwd, settled).await)
        })
    }
}
