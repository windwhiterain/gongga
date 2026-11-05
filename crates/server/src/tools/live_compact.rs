//! The agent-written checkpoint artifact; the agent loop commits it with its result.

use serde_json::{Value, json};

use crate::tools::{Tool, ToolContext, ToolOutcome};

pub const LIVE_COMPACT_TOOL: &str = "live_compact";

pub struct LiveCompactTool;

impl Tool for LiveCompactTool {
    fn name(&self) -> &'static str {
        LIVE_COMPACT_TOOL
    }

    fn description(&self) -> &'static str {
        "Write the checkpoint summary requested by the server. Summarize everything since \
         the previous checkpoint, including the latest ordinary tool-call round. The server \
         retains that complete round separately and keeps every checkpoint summary."
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
        _ctx: &'a ToolContext,
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
            ToolOutcome::ok(summary)
        })
    }
}
