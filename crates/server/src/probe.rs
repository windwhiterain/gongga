//! Aggregated timing and size measurements for one HTTP response attempt.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One attempt's measurements. Deltas are aggregated in memory and written once.
#[derive(Debug, Clone)]
pub struct AttemptProbe {
    build_head_ms: u64,
    transcript_open_ms: u64,
    head_bytes: usize,
    started_at: i64,
    started: Instant,
    status: Option<u16>,
    outcome: String,
    request_bytes: usize,
    headers_ms: Option<u64>,
    first_chunk_ms: Option<u64>,
    last_chunk_ms: Option<u64>,
    first_event_ms: Option<u64>,
    last_event_ms: Option<u64>,
    first_reasoning_ms: Option<u64>,
    last_reasoning_ms: Option<u64>,
    first_output_ms: Option<u64>,
    last_output_ms: Option<u64>,
    first_tool_arguments_ms: Option<u64>,
    last_tool_arguments_ms: Option<u64>,
    raw_chunks: u64,
    raw_bytes: u64,
    sse_events: u64,
    tail_events: u64,
    parse_errors: u64,
    process_total_us: u64,
    process_max_us: u64,
    event_kinds: HashMap<String, u64>,
    reasoning_items: u64,
    reasoning_deltas: u64,
    reasoning_chars: u64,
    output_items: u64,
    output_deltas: u64,
    output_chars: u64,
    tool_items: u64,
    tool_argument_deltas: u64,
    tool_argument_chars: u64,
    provider_items: u64,
    usage: Option<Value>,
}

impl AttemptProbe {
    pub fn new(build_head_ms: u64, transcript_open_ms: u64, head_bytes: usize) -> Self {
        Self {
            build_head_ms,
            transcript_open_ms,
            head_bytes,
            started_at: crate::store::now_millis(),
            started: Instant::now(),
            status: None,
            outcome: "ended".to_string(),
            request_bytes: 0,
            headers_ms: None,
            first_chunk_ms: None,
            last_chunk_ms: None,
            first_event_ms: None,
            last_event_ms: None,
            first_reasoning_ms: None,
            last_reasoning_ms: None,
            first_output_ms: None,
            last_output_ms: None,
            first_tool_arguments_ms: None,
            last_tool_arguments_ms: None,
            raw_chunks: 0,
            raw_bytes: 0,
            sse_events: 0,
            tail_events: 0,
            parse_errors: 0,
            process_total_us: 0,
            process_max_us: 0,
            event_kinds: HashMap::new(),
            reasoning_items: 0,
            reasoning_deltas: 0,
            reasoning_chars: 0,
            output_items: 0,
            output_deltas: 0,
            output_chars: 0,
            tool_items: 0,
            tool_argument_deltas: 0,
            tool_argument_chars: 0,
            provider_items: 0,
            usage: None,
        }
    }

    fn ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn outcome(&mut self, outcome: &str) {
        self.outcome = outcome.to_string();
    }

    pub fn headers(&mut self, status: reqwest::StatusCode, request_bytes: usize) {
        self.status = Some(status.as_u16());
        self.request_bytes = request_bytes;
        self.headers_ms = Some(self.ms());
    }

    pub fn chunk(&mut self, bytes: usize) {
        let at = self.ms();
        self.first_chunk_ms.get_or_insert(at);
        self.last_chunk_ms = Some(at);
        self.raw_chunks += 1;
        self.raw_bytes += bytes as u64;
    }

    pub fn process(&mut self, elapsed: Duration) {
        let micros = elapsed.as_micros() as u64;
        self.process_total_us += micros;
        self.process_max_us = self.process_max_us.max(micros);
    }

    pub fn event(&mut self, name: &str, data: &Value, tail: bool) {
        let at = self.ms();
        self.first_event_ms.get_or_insert(at);
        self.last_event_ms = Some(at);
        self.sse_events += 1;
        self.tail_events += u64::from(tail);
        self.parse_errors += u64::from(data.is_null() && name != "[DONE]");
        *self.event_kinds.entry(name.to_string()).or_default() += 1;

        let delta = data["delta"].as_str().unwrap_or_default();
        match name {
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.first_reasoning_ms.get_or_insert(at);
                self.last_reasoning_ms = Some(at);
                self.reasoning_deltas += 1;
                self.reasoning_chars += delta.chars().count() as u64;
            }
            "response.output_text.delta" => {
                self.first_output_ms.get_or_insert(at);
                self.last_output_ms = Some(at);
                self.output_deltas += 1;
                self.output_chars += delta.chars().count() as u64;
            }
            "response.function_call_arguments.delta" => {
                self.first_tool_arguments_ms.get_or_insert(at);
                self.last_tool_arguments_ms = Some(at);
                self.tool_argument_deltas += 1;
                self.tool_argument_chars += delta.chars().count() as u64;
            }
            "response.output_item.added" => match data["item"]["type"].as_str().unwrap_or_default()
            {
                "reasoning" => self.reasoning_items += 1,
                "message" => self.output_items += 1,
                "function_call" => self.tool_items += 1,
                kind if kind.ends_with("_call") => self.provider_items += 1,
                _ => {}
            },
            "response.completed" => {
                self.usage = Some(data["response"]["usage"].clone());
            }
            _ => {}
        }
    }

    pub fn to_json(self) -> Value {
        let elapsed_ms = self.ms();
        json!({
            "build_head_ms": self.build_head_ms,
            "transcript_open_ms": self.transcript_open_ms,
            "head_bytes": self.head_bytes,
            "request_bytes": self.request_bytes,
            "started_at": self.started_at,
            "ended_at": crate::store::now_millis(),
            "elapsed_ms": elapsed_ms,
            "outcome": self.outcome,
            "http_status": self.status,
            "headers_ms": self.headers_ms,
            "first_chunk_ms": self.first_chunk_ms,
            "last_chunk_ms": self.last_chunk_ms,
            "first_event_ms": self.first_event_ms,
            "last_event_ms": self.last_event_ms,
            "first_reasoning_delta_ms": self.first_reasoning_ms,
            "last_reasoning_delta_ms": self.last_reasoning_ms,
            "first_output_delta_ms": self.first_output_ms,
            "last_output_delta_ms": self.last_output_ms,
            "first_tool_arguments_delta_ms": self.first_tool_arguments_ms,
            "last_tool_arguments_delta_ms": self.last_tool_arguments_ms,
            "raw_chunks": self.raw_chunks,
            "raw_bytes": self.raw_bytes,
            "sse_events": self.sse_events,
            "tail_events": self.tail_events,
            "parse_errors": self.parse_errors,
            "process_total_us": self.process_total_us,
            "process_max_us": self.process_max_us,
            "event_kinds": self.event_kinds,
            "reasoning_items": self.reasoning_items,
            "reasoning_deltas": self.reasoning_deltas,
            "reasoning_chars": self.reasoning_chars,
            "output_items": self.output_items,
            "output_deltas": self.output_deltas,
            "output_chars": self.output_chars,
            "tool_items": self.tool_items,
            "tool_argument_deltas": self.tool_argument_deltas,
            "tool_argument_chars": self.tool_argument_chars,
            "provider_items": self.provider_items,
            "usage": self.usage,
        })
    }
}
