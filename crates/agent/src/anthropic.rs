//! Anthropic `Messages API` provider adapter.
//!
//! Maps [`crate::Provider::stream`] onto a single POST to `/v1/messages?stream=true` and
//! translates the server-sent-events stream into [`ProviderEvent`]s. The agent loop never
//! sees Anthropic content blocks — they are buffered and emitted as complete `Text` deltas
//! and finalised `ToolCall` events here.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, trace, warn};

use crate::error::{ProviderError, ProviderResult};
use crate::provider::Provider;
use crate::types::{Message, MessageContent, ProviderEvent, Role, StopReason, ToolDef};

/// Default model id. Picked to match `docs/ARCHITECTURE.md` "Default base models" plus the
/// general-purpose Claude family used for the agent loop in Phase 1.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Default base URL — overridable for tests / proxies.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// API version header expected by `/v1/messages`.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default budget for one assistant turn. Roughly 12k tokens of reply, conservative.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// HTTP timeout for the *connection* phase; the SSE body itself is allowed to stream
/// indefinitely once the response headers arrive.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Construct an adapter using sensible defaults — only the API key is required.
    pub fn new(api_key: impl Into<String>) -> ProviderResult<Self> {
        Self::builder().api_key(api_key).build()
    }

    #[must_use]
    pub fn builder() -> AnthropicProviderBuilder {
        AnthropicProviderBuilder::default()
    }
}

#[derive(Debug, Default)]
pub struct AnthropicProviderBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
}

impl AnthropicProviderBuilder {
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
    #[must_use]
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn build(self) -> ProviderResult<AnthropicProvider> {
        let api_key = self
            .api_key
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ProviderError::Config("missing Anthropic API key".into()))?;

        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("narrowmind-agent/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(AnthropicProvider {
            api_key,
            base_url: self.base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model: self.model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            max_tokens: self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            client,
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn stream(
        &self,
        messages: Vec<Message>,
        tools: &[ToolDef],
    ) -> ProviderResult<BoxStream<'static, ProviderEvent>> {
        let request = build_request_body(&self.model, self.max_tokens, &messages, tools);
        trace!(?request, "anthropic request");

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("x-api-key", header_value(&self.api_key)?);
        headers.insert("anthropic-version", HeaderValue::from_static(ANTHROPIC_VERSION));
        headers.insert("accept", HeaderValue::from_static("text/event-stream"));

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let response = self.client.post(&url).headers(headers).json(&request).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body: truncate(&body, 4096),
            });
        }

        let (tx, rx) = mpsc::unbounded_channel::<ProviderEvent>();
        tokio::spawn(pump_sse(response, tx));
        Ok(UnboundedReceiverStream::new(rx).boxed())
    }
}

fn header_value(s: &str) -> ProviderResult<HeaderValue> {
    HeaderValue::from_str(s).map_err(|e| ProviderError::Config(format!("invalid header: {e}")))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

// ---------------------------------------------------------------------------
// Request shaping
// ---------------------------------------------------------------------------

fn build_request_body(model: &str, max_tokens: u32, messages: &[Message], tools: &[ToolDef]) -> Value {
    let mut system_text = String::new();
    let mut converted: Vec<Value> = Vec::with_capacity(messages.len());
    for m in messages {
        match m.role {
            Role::System => {
                for c in &m.content {
                    if let MessageContent::Text { text } = c {
                        if !system_text.is_empty() {
                            system_text.push_str("\n\n");
                        }
                        system_text.push_str(text);
                    }
                }
            }
            Role::User | Role::Assistant => {
                converted.push(serde_json::json!({
                    "role": match m.role { Role::User => "user", _ => "assistant" },
                    "content": m.content.iter().map(to_wire_content).collect::<Vec<_>>(),
                }));
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": converted,
        "stream": true,
    });
    if !system_text.is_empty() {
        body["system"] = Value::String(system_text);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect(),
        );
    }
    body
}

fn to_wire_content(c: &MessageContent) -> Value {
    match c {
        MessageContent::Text { text } => {
            serde_json::json!({ "type": "text", "text": text })
        }
        MessageContent::ToolUse { id, name, input } => {
            serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            })
        }
        MessageContent::ToolResult { tool_use_id, content, is_error } => {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// SSE pump
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct StreamState {
    /// Buffered partial `input_json` text per `content_block` index. Cleared on block stop.
    tool_inputs: HashMap<u32, ToolInputBuffer>,
    /// Final stop reason captured from `message_delta`.
    stop_reason: Option<StopReason>,
    /// Token accounting. `message_start` carries `usage.input_tokens`;
    /// `message_delta` carries a CUMULATIVE `usage.output_tokens` that we
    /// overwrite each time so the last value is the message total. Folded
    /// into a single `ProviderEvent::Usage` right before `Stop`.
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Debug, Default)]
struct ToolInputBuffer {
    id: String,
    name: String,
    json: String,
}

/// Drive the SSE body, parse events, and forward translated `ProviderEvent`s on `tx`.
async fn pump_sse(response: reqwest::Response, tx: mpsc::UnboundedSender<ProviderEvent>) {
    let mut state = StreamState::default();
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut byte_stream = response.bytes_stream();

    while let Some(chunk) = byte_stream.next().await {
        match chunk {
            Ok(bytes) => {
                buf.extend_from_slice(&bytes);
                drain_events(&mut buf, &mut state, &tx);
            }
            Err(e) => {
                warn!(error = %e, "SSE byte stream error");
                let _ = tx.send(ProviderEvent::Stop {
                    reason: StopReason::Error(format!("stream byte error: {e}")),
                });
                return;
            }
        }
    }

    // Flush any partially-buffered event left in `buf` after the body closes.
    drain_events(&mut buf, &mut state, &tx);

    // Per the ProviderEvent::Usage contract: at most one Usage event, final
    // totals, immediately before Stop. Skipped when the stream died before
    // message_start (both still zero) so consumers never see a bogus 0/0.
    if state.input_tokens > 0 || state.output_tokens > 0 {
        let _ = tx.send(ProviderEvent::Usage {
            input_tokens: state.input_tokens,
            output_tokens: state.output_tokens,
        });
    }

    let reason = state.stop_reason.unwrap_or_else(|| {
        StopReason::Error("stream closed without message_stop".into())
    });
    let _ = tx.send(ProviderEvent::Stop { reason });
}

/// Consume complete SSE messages from `buf` (each terminated by `\n\n`) and dispatch them.
fn drain_events(
    buf: &mut Vec<u8>,
    state: &mut StreamState,
    tx: &mpsc::UnboundedSender<ProviderEvent>,
) {
    loop {
        let Some(boundary) = find_event_boundary(buf) else {
            return;
        };
        // Move the event bytes out of buf.
        let event_bytes: Vec<u8> = buf.drain(..boundary.end).collect();
        let event_str = std::str::from_utf8(&event_bytes[..boundary.payload_len]).unwrap_or("");
        if event_str.is_empty() {
            continue;
        }
        if let Some(parsed) = parse_sse_block(event_str) {
            dispatch(&parsed, state, tx);
        }
    }
}

#[derive(Debug)]
struct EventBoundary {
    /// Length of the payload portion before the blank-line separator.
    payload_len: usize,
    /// Total bytes to drain (payload + separator).
    end: usize,
}

fn find_event_boundary(buf: &[u8]) -> Option<EventBoundary> {
    // SSE events end on a blank line: either "\n\n" or "\r\n\r\n".
    if let Some(pos) = find_subsequence(buf, b"\n\n") {
        return Some(EventBoundary { payload_len: pos, end: pos + 2 });
    }
    if let Some(pos) = find_subsequence(buf, b"\r\n\r\n") {
        return Some(EventBoundary { payload_len: pos, end: pos + 4 });
    }
    None
}

fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Debug, Default)]
struct ParsedSse {
    event: Option<String>,
    data: String,
}

fn parse_sse_block(block: &str) -> Option<ParsedSse> {
    let mut out = ParsedSse::default();
    for line in block.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.trim_start_matches(' ')),
            None => (line, ""),
        };
        match field {
            "event" => out.event = Some(value.to_string()),
            "data" => {
                if !out.data.is_empty() {
                    out.data.push('\n');
                }
                out.data.push_str(value);
            }
            _ => { /* id / retry — not used */ }
        }
    }
    if out.data.is_empty() && out.event.is_none() {
        None
    } else {
        Some(out)
    }
}

fn dispatch(ev: &ParsedSse, state: &mut StreamState, tx: &mpsc::UnboundedSender<ProviderEvent>) {
    let payload: Value = match serde_json::from_str(&ev.data) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, data = %ev.data, "skipping unparseable SSE data");
            return;
        }
    };
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    debug!(event = ?ev.event, kind, "sse event");

    match kind {
        "ping" | "message_stop" => { /* no-op */ }

        "message_start" => {
            // The opening frame carries the request's input-token count (and an
            // initial output count) under message.usage.
            if let Some(usage) = payload.get("message").and_then(|m| m.get("usage")) {
                if let Some(n) = usage.get("input_tokens").and_then(Value::as_u64) {
                    state.input_tokens = n;
                }
                if let Some(n) = usage.get("output_tokens").and_then(Value::as_u64) {
                    state.output_tokens = n;
                }
            }
        }

        "content_block_start" => {
            let Some(block) = payload.get("content_block") else { return };
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let Some(index) = payload.get("index").and_then(Value::as_u64) else { return };
                let id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                let name = block.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                state.tool_inputs.insert(
                    u32::try_from(index).unwrap_or_default(),
                    ToolInputBuffer { id, name, json: String::new() },
                );
            }
        }

        "content_block_delta" => {
            let Some(delta) = payload.get("delta") else { return };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        let _ = tx.send(ProviderEvent::Text(text.to_string()));
                    }
                }
                Some("input_json_delta") => {
                    let Some(index) = payload.get("index").and_then(Value::as_u64) else { return };
                    let key = u32::try_from(index).unwrap_or_default();
                    if let Some(buf) = state.tool_inputs.get_mut(&key) {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            buf.json.push_str(partial);
                        }
                    }
                }
                _ => { /* thinking deltas etc. */ }
            }
        }

        "content_block_stop" => {
            let Some(index) = payload.get("index").and_then(Value::as_u64) else { return };
            let key = u32::try_from(index).unwrap_or_default();
            if let Some(buf) = state.tool_inputs.remove(&key) {
                // tool_use blocks have `input_json_delta` chunks even when empty — empty
                // body legitimately means an empty JSON object.
                let raw = if buf.json.is_empty() { "{}".to_string() } else { buf.json };
                match serde_json::from_str::<Value>(&raw) {
                    Ok(input) => {
                        let _ = tx.send(ProviderEvent::ToolCall {
                            id: buf.id,
                            name: buf.name,
                            input,
                        });
                        let _ = tx.send(ProviderEvent::ToolResultRequested);
                    }
                    Err(e) => {
                        warn!(error = %e, raw, "tool_use input json was malformed");
                        let _ = tx.send(ProviderEvent::Stop {
                            reason: StopReason::Error(format!("tool_use input parse: {e}")),
                        });
                    }
                }
            }
        }

        "message_delta" => {
            if let Some(reason) = payload
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                state.stop_reason = Some(stop_reason_from_wire(reason));
            }
            // usage.output_tokens here is cumulative for the message — overwrite,
            // never add, so the last delta leaves the final total in state.
            if let Some(n) = payload
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64)
            {
                state.output_tokens = n;
            }
        }

        "error" => {
            let msg = payload
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown anthropic error")
                .to_string();
            let _ = tx.send(ProviderEvent::Stop { reason: StopReason::Error(msg) });
        }

        _ => trace!(?ev, "ignoring unknown SSE event"),
    }
}

fn stop_reason_from_wire(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        other => StopReason::Error(format!("unknown stop_reason: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Internal helper exposed only for tests below.
#[cfg(test)]
fn make_test_stream(sse_chunks: Vec<&'static str>) -> BoxStream<'static, ProviderEvent> {
    use bytes::Bytes;
    use futures::stream;

    let byte_chunks = sse_chunks
        .into_iter()
        .map(|s| Ok::<_, std::io::Error>(Bytes::from(s.as_bytes())));
    let body_stream = stream::iter(byte_chunks);
    let (tx, rx) = mpsc::unbounded_channel::<ProviderEvent>();

    tokio::spawn(async move {
        let mut state = StreamState::default();
        let mut buf: Vec<u8> = Vec::new();
        let mut s = body_stream;
        while let Some(chunk) = s.next().await {
            match chunk {
                Ok(bytes) => {
                    buf.extend_from_slice(&bytes);
                    drain_events(&mut buf, &mut state, &tx);
                }
                Err(e) => {
                    let _ = tx.send(ProviderEvent::Stop {
                        reason: StopReason::Error(format!("test stream err: {e}")),
                    });
                    return;
                }
            }
        }
        drain_events(&mut buf, &mut state, &tx);
        let reason = state.stop_reason.unwrap_or(StopReason::EndTurn);
        let _ = tx.send(ProviderEvent::Stop { reason });
    });

    UnboundedReceiverStream::new(rx).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn parses_plain_text_response() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\"}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut s = make_test_stream(vec![sse]);
        let mut got: Vec<ProviderEvent> = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev);
        }
        assert_eq!(got.len(), 3, "events: {got:?}");
        assert!(matches!(&got[0], ProviderEvent::Text(t) if t == "Hello "));
        assert!(matches!(&got[1], ProviderEvent::Text(t) if t == "world"));
        assert!(matches!(&got[2], ProviderEvent::Stop { reason: StopReason::EndTurn }));
    }

    #[tokio::test]
    async fn parses_tool_use_response() {
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":\\\"a.txt\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut s = make_test_stream(vec![sse]);
        let mut got: Vec<ProviderEvent> = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev);
        }
        assert!(matches!(&got[0], ProviderEvent::ToolCall { id, name, input }
            if id == "tu_1" && name == "read_file" && input["path"] == "a.txt"));
        assert!(matches!(&got[1], ProviderEvent::ToolResultRequested));
        assert!(matches!(got.last().unwrap(), ProviderEvent::Stop { reason: StopReason::ToolUse }));
    }

    #[tokio::test]
    async fn parses_chunked_event_split_across_byte_chunks() {
        let part1 = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}";
        let part2 = "\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n";
        let mut s = make_test_stream(vec![part1, part2]);
        let mut texts = Vec::new();
        let mut stop = None;
        while let Some(ev) = s.next().await {
            match ev {
                ProviderEvent::Text(t) => texts.push(t),
                ProviderEvent::Stop { reason } => stop = Some(reason),
                _ => {}
            }
        }
        assert_eq!(texts, vec!["hi".to_string()]);
        assert_eq!(stop, Some(StopReason::EndTurn));
    }

    #[test]
    fn build_request_strips_system_and_serialises_tools() {
        let messages = vec![
            Message::system("you are helpful"),
            Message::user("hi"),
        ];
        let tools = vec![ToolDef {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let body = build_request_body("claude-test", 1024, &messages, &tools);
        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "you are helpful");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["tools"][0]["name"], "read_file");
    }

    #[test]
    fn builder_rejects_empty_api_key() {
        let err = AnthropicProvider::builder().api_key("").build().unwrap_err();
        assert!(matches!(err, ProviderError::Config(_)));
    }
}
