//! Conversation, content-block, and event types shared by every `Provider` impl.
//!
//! The split between [`Message`] / [`MessageContent`] (input to a provider) and
//! [`ProviderEvent`] (output from a provider) is intentional. Providers receive structured
//! conversation history and emit a format-agnostic event stream the orchestrator routes —
//! the orchestrator never sees `Anthropic` content blocks, `OpenAI` choices, or `Ollama` deltas.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Conversation role. `System` becomes the top-level `system` field for Anthropic providers
/// and the first system message for OpenAI-shaped providers; adapters handle the mapping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One block of content inside a message. Tool use and tool result blocks are first-class so
/// the agent loop can carry tool calls forward in subsequent provider turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    /// Plain text from the user, system, or assistant.
    Text { text: String },
    /// Assistant requested a tool call. `input` is a JSON object matching the tool's schema.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Tool result fed back into the conversation for the next assistant turn.
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// A conversation message. `content` may hold multiple blocks (e.g. text + tool use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<MessageContent>,
}

impl Message {
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![MessageContent::Text { text: text.into() }],
        }
    }

    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![MessageContent::Text { text: text.into() }],
        }
    }

    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![MessageContent::Text { text: text.into() }],
        }
    }
}

/// Description of a tool the agent is allowed to call. `input_schema` is JSON Schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Why the model stopped producing tokens this turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model finished its turn normally.
    EndTurn,
    /// Hit the configured `max_tokens` budget.
    MaxTokens,
    /// Stopped on a user-specified stop sequence.
    StopSequence,
    /// Model emitted a tool use and is waiting for tool results.
    ToolUse,
    /// Stream ended on a streaming-level error (network drop, malformed event, ...).
    /// The string carries a human-readable reason for surfacing in logs / UI.
    Error(String),
}

/// Format-agnostic event the agent loop consumes. Every `Provider` impl maps its vendor
/// stream into this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    /// Incremental text delta. Concatenate to render the assistant's reply.
    Text(String),
    /// A single tool call request. `input` is the *complete* arguments object — adapters
    /// buffer partial argument JSON internally and emit `ToolCall` once on close.
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// Marker emitted by the adapter right after a `ToolCall` to signal the agent loop
    /// should run the tool and feed a `ToolResult` block back before resuming streaming.
    ToolResultRequested,
    /// Stream terminated. No further events.
    Stop { reason: StopReason },
}
