//! The agent-written checkpoint artifact; the agent loop commits it with its result.

use serde_json::{Value, json};

use crate::tools::{Tool, ToolContext, ToolOutcome};

pub const LIVE_COMPACT_TOOL: &str = "live_compact";

pub struct LiveCompactTool;

impl Tool for LiveCompactTool {
    fn name(&self) -> &'static str {
        LIVE_COMPACT_TOOL
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "The agent-written summary of the segment since the previous checkpoint."
                }
            },
            "required": ["summary"],
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let summary = args
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if summary.is_empty() {
                return ToolOutcome::err("summary must not be empty");
            }
            // Where this session's references are read: the machine it stands on.
            let source = match (&ctx.reader, ctx.machine) {
                (Some(reader), crate::session::Machine::Remote) => {
                    crate::meta::Source::Remote(reader)
                }
                (None, crate::session::Machine::Remote) => crate::meta::Source::Unreachable,
                (_, crate::session::Machine::Local) => crate::meta::Source::Local,
            };
            ToolOutcome::ok(crate::meta::embed(source, &ctx.cwd, summary).await)
        })
    }
}
