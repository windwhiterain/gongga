//! Agent-facing memory checkpoints and explicit recalls.

use serde_json::Value;

use crate::tools::{Cmd, ToolContext, ToolOutcome};

pub const MEMORIZE_TOOL: &str = "memorize";
pub const RECALL_TOOL: &str = "recall";

pub struct MemorizeTool;
pub struct RecallTool;

impl Cmd for MemorizeTool {
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
            let space = ctx.space();
            let machines = crate::meta::Machines::of(&space);
            ToolOutcome::ok(crate::meta::embed(&machines, &ctx.cwd, summary).await)
        })
    }
}

impl Cmd for RecallTool {
    fn run<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(ids) = args.get("ids").and_then(Value::as_array) else {
                return ToolOutcome::err("ids must be a non-empty list of memory ids");
            };
            if ids.is_empty()
                || ids
                    .iter()
                    .any(|id| id.as_str().is_none_or(|id| id.trim().is_empty()))
            {
                return ToolOutcome::err("ids must be a non-empty list of memory ids");
            }
            ToolOutcome::ok("recall requested".to_string())
        })
    }
}
