//! `AgentSession` — the Claude Code–style agent loop.
//!
//! One session = one conversation. Calling [`AgentSession::send_user_message`] appends a
//! user message and runs as many provider turns as needed until the model stops requesting
//! tools. Streaming events (assistant text deltas, tool boundaries, stop reasons) flow out
//! through an mpsc channel the UI subscribes to.
//!
//! Loop safety: a max iterations counter prevents runaway tool/tool-result ping-pong.

use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::dispatcher::{ToolDispatchOutcome, ToolDispatcher};
use crate::error::ProviderError;
use crate::provider::Provider;
use crate::types::{Message, MessageContent, ProviderEvent, Role, StopReason};

/// How many provider→tool→provider cycles a single `send_user_message` is allowed to drive
/// before we bail out. Anthropic recommends 10–20; we cap a little higher.
pub const MAX_TURN_ITERATIONS: u32 = 25;

/// Errors surfaced from the agent loop. Streaming-level provider errors do *not* land here —
/// they're emitted as `AgentEvent::Stop { reason: Error(_) }` so the UI can render them in
/// the transcript instead of unwinding the call.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error("agent loop hit the {MAX_TURN_ITERATIONS}-iteration safety cap")]
    LoopLimitExceeded,
}

/// Token accounting for one provider iteration. Sum across `TurnEnd` events to get a
/// request or session total — each iteration reports only its own stream's usage, so
/// plain addition never double-counts.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Event the agent loop emits as it drives a turn. Each variant maps onto something the UI
/// will render in the transcript.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Incremental assistant text. Concatenate to render the in-progress message.
    AssistantTextDelta { text: String },
    /// Assistant emitted a `tool_use` block. The loop is about to dispatch the tool.
    ToolCallStart { id: String, name: String, input: Value },
    /// Tool invocation finished. `content` is what the model will see; `is_error` matches
    /// what we'll put on the `tool_result` block.
    ToolCallEnd {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    /// One full assistant turn ended. `reason` is the upstream provider's stop reason.
    /// `more_turns` indicates whether the loop will continue (true after a `tool_use`).
    /// `usage` covers THIS provider iteration only (zeroes when the provider doesn't
    /// report usage) — UIs sum across events for running totals.
    TurnEnd {
        reason: StopReason,
        more_turns: bool,
        usage: TokenUsage,
    },
}

/// Live agent loop. Holds the provider, the tool dispatcher, and the conversation history.
pub struct AgentSession {
    provider: Arc<dyn Provider>,
    dispatcher: Arc<dyn ToolDispatcher>,
    system_prompt: Option<String>,
    messages: Vec<Message>,
    events: UnboundedSender<AgentEvent>,
}

impl AgentSession {
    pub fn new(
        provider: Arc<dyn Provider>,
        dispatcher: Arc<dyn ToolDispatcher>,
        system_prompt: Option<String>,
        events: UnboundedSender<AgentEvent>,
    ) -> Self {
        Self::with_messages(provider, dispatcher, system_prompt, events, Vec::new())
    }

    /// Resume a conversation with an existing message history.
    pub fn with_messages(
        provider: Arc<dyn Provider>,
        dispatcher: Arc<dyn ToolDispatcher>,
        system_prompt: Option<String>,
        events: UnboundedSender<AgentEvent>,
        messages: Vec<Message>,
    ) -> Self {
        Self {
            provider,
            dispatcher,
            system_prompt,
            messages,
            events,
        }
    }

    /// All messages in the conversation so far. Includes the synthesised `system` message
    /// (if a prompt was given) plus every user/assistant exchange.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Drop the conversation history. The system prompt is preserved.
    pub fn reset(&mut self) {
        self.messages.clear();
    }

    /// Number of messages currently in the conversation (excluding the auto-prepended system).
    #[must_use]
    pub fn turn_count(&self) -> usize {
        self.messages.len()
    }

    /// Append a user message and drive the loop until the model stops asking for tools.
    pub async fn send_user_message(&mut self, text: impl Into<String>) -> Result<(), AgentError> {
        self.messages.push(Message::user(text.into()));
        self.run().await
    }

    async fn run(&mut self) -> Result<(), AgentError> {
        for iteration in 0..MAX_TURN_ITERATIONS {
            let outgoing = self.build_outgoing_messages();
            let tools = self.dispatcher.defs();
            debug!(iteration, msg_count = outgoing.len(), tools = tools.len(), "agent turn");

            let mut stream = self.provider.stream(outgoing, &tools).await?;

            let mut assistant_text = String::new();
            let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();
            let mut stop_reason = StopReason::EndTurn;
            let mut usage = TokenUsage::default();

            while let Some(event) = stream.next().await {
                match event {
                    ProviderEvent::Text(delta) => {
                        assistant_text.push_str(&delta);
                        self.emit(AgentEvent::AssistantTextDelta { text: delta });
                    }
                    ProviderEvent::ToolCall { id, name, input } => {
                        pending_tool_calls.push(PendingToolCall { id, name, input });
                    }
                    ProviderEvent::ToolResultRequested => {
                        // Marker only — Anthropic's adapter emits this after each tool_use
                        // block. Loop continues regardless.
                    }
                    ProviderEvent::Usage { input_tokens, output_tokens } => {
                        // At most one per stream (adapter contract) — plain assignment.
                        usage = TokenUsage { input_tokens, output_tokens };
                    }
                    ProviderEvent::Stop { reason } => {
                        stop_reason = reason;
                        break;
                    }
                }
            }

            // Persist the assistant turn (text + any tool_use blocks) before dispatching tools
            // so the conversation history is consistent if dispatching panics.
            self.append_assistant_turn(&assistant_text, &pending_tool_calls);

            if pending_tool_calls.is_empty() {
                // Final assistant turn — model is done.
                self.emit(AgentEvent::TurnEnd {
                    reason: stop_reason,
                    more_turns: false,
                    usage,
                });
                return Ok(());
            }

            // Run every requested tool, surfacing each as a (tool_call_start, tool_call_end)
            // pair to the UI, and accumulate the tool_result blocks for the next provider turn.
            let mut tool_result_blocks: Vec<MessageContent> = Vec::with_capacity(pending_tool_calls.len());
            for call in pending_tool_calls {
                self.emit(AgentEvent::ToolCallStart {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                });
                let outcome: ToolDispatchOutcome =
                    self.dispatcher.dispatch(&call.name, call.input.clone()).await;
                info!(
                    tool = %call.name,
                    id = %call.id,
                    is_error = outcome.is_error,
                    content_len = outcome.content.len(),
                    "dispatched tool",
                );
                self.emit(AgentEvent::ToolCallEnd {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    content: outcome.content.clone(),
                    is_error: outcome.is_error,
                });
                tool_result_blocks.push(MessageContent::ToolResult {
                    tool_use_id: call.id,
                    content: outcome.content,
                    is_error: outcome.is_error,
                });
            }
            self.messages.push(Message {
                role: Role::User,
                content: tool_result_blocks,
            });

            self.emit(AgentEvent::TurnEnd {
                reason: stop_reason,
                more_turns: true,
                usage,
            });
        }

        warn!(cap = MAX_TURN_ITERATIONS, "agent loop hit safety cap");
        Err(AgentError::LoopLimitExceeded)
    }

    /// Construct the message vector sent to the provider for this turn:
    /// system prompt (if any), then the full conversation so far.
    fn build_outgoing_messages(&self) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::with_capacity(self.messages.len() + 1);
        if let Some(system) = &self.system_prompt {
            out.push(Message::system(system.clone()));
        }
        out.extend_from_slice(&self.messages);
        out
    }

    fn append_assistant_turn(&mut self, text: &str, tool_calls: &[PendingToolCall]) {
        let mut content: Vec<MessageContent> = Vec::new();
        if !text.is_empty() {
            content.push(MessageContent::Text {
                text: text.to_string(),
            });
        }
        for c in tool_calls {
            content.push(MessageContent::ToolUse {
                id: c.id.clone(),
                name: c.name.clone(),
                input: c.input.clone(),
            });
        }
        if !content.is_empty() {
            self.messages.push(Message {
                role: Role::Assistant,
                content,
            });
        }
    }

    fn emit(&self, event: AgentEvent) {
        // Receivers dropping is expected when the UI navigates away mid-turn;
        // we don't need to abort the loop for it.
        if self.events.send(event).is_err() {
            debug!("agent event receiver dropped; continuing");
        }
    }
}

/// Fresh ULID-ish identifier suitable for `tool_use_id` when a provider has not supplied one.
/// Currently unused — Anthropic always supplies an id — but kept for non-Anthropic adapters.
#[must_use]
pub fn fresh_tool_use_id() -> String {
    format!("call_{}", Uuid::new_v4().simple())
}

#[derive(Debug)]
struct PendingToolCall {
    id: String,
    name: String,
    input: Value,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures::stream::{self, BoxStream, StreamExt};
    use tokio::sync::mpsc;

    use super::*;
    use crate::dispatcher::{ToolDispatchOutcome, ToolDispatcher};
    use crate::error::{ProviderError, ProviderResult};
    use crate::provider::Provider;
    use crate::types::{ProviderEvent, StopReason, ToolDef};

    /// Provider double: scripted with a queue of pre-built event lists. Each call to
    /// `stream()` consumes one entry.
    struct ScriptedProvider {
        plan: Mutex<Vec<Vec<ProviderEvent>>>,
    }

    impl ScriptedProvider {
        fn new(plan: Vec<Vec<ProviderEvent>>) -> Self {
            Self {
                plan: Mutex::new(plan),
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn name(&self) -> &'static str {
            "scripted"
        }

        async fn stream(
            &self,
            _messages: Vec<Message>,
            _tools: &[ToolDef],
        ) -> ProviderResult<BoxStream<'static, ProviderEvent>> {
            let mut plan = self.plan.lock().unwrap();
            if plan.is_empty() {
                return Err(ProviderError::Malformed("scripted provider exhausted".into()));
            }
            let events = plan.remove(0);
            Ok(stream::iter(events).boxed())
        }
    }

    struct ScriptedDispatcher {
        defs: Vec<ToolDef>,
        outcomes: Mutex<Vec<ToolDispatchOutcome>>,
        last_calls: Mutex<Vec<(String, Value)>>,
    }

    #[async_trait]
    impl ToolDispatcher for ScriptedDispatcher {
        fn defs(&self) -> Vec<ToolDef> {
            self.defs.clone()
        }

        async fn dispatch(&self, name: &str, args: Value) -> ToolDispatchOutcome {
            self.last_calls.lock().unwrap().push((name.into(), args));
            self.outcomes
                .lock()
                .unwrap()
                .remove(0)
        }
    }

    fn collect(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[tokio::test]
    async fn one_turn_no_tools_emits_text_and_end_turn() {
        let provider = Arc::new(ScriptedProvider::new(vec![vec![
            ProviderEvent::Text("Hello, ".into()),
            ProviderEvent::Text("world.".into()),
            ProviderEvent::Stop { reason: StopReason::EndTurn },
        ]]));
        let dispatcher = Arc::new(ScriptedDispatcher {
            defs: vec![],
            outcomes: Mutex::new(vec![]),
            last_calls: Mutex::new(vec![]),
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut session = AgentSession::new(provider, dispatcher, None, tx);
        session.send_user_message("hi").await.unwrap();

        let events = collect(&mut rx);
        // 2 deltas + 1 turn-end
        assert_eq!(events.len(), 3);
        match &events[0] {
            AgentEvent::AssistantTextDelta { text } => assert_eq!(text, "Hello, "),
            other => panic!("expected delta, got {other:?}"),
        }
        match &events[2] {
            AgentEvent::TurnEnd { reason, more_turns, .. } => {
                assert_eq!(reason, &StopReason::EndTurn);
                assert!(!more_turns);
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
        // conversation has user + assistant
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, Role::User);
        assert_eq!(session.messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn tool_call_round_trip_appends_result_and_continues_loop() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            // Turn 1: model asks for a tool
            vec![
                ProviderEvent::ToolCall {
                    id: "tc_1".into(),
                    name: "echo".into(),
                    input: serde_json::json!({"text": "hi"}),
                },
                ProviderEvent::ToolResultRequested,
                ProviderEvent::Stop { reason: StopReason::ToolUse },
            ],
            // Turn 2: model responds after seeing the tool_result
            vec![
                ProviderEvent::Text("done".into()),
                ProviderEvent::Stop { reason: StopReason::EndTurn },
            ],
        ]));
        let dispatcher = Arc::new(ScriptedDispatcher {
            defs: vec![ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            outcomes: Mutex::new(vec![ToolDispatchOutcome::ok("hi")]),
            last_calls: Mutex::new(vec![]),
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut session = AgentSession::new(provider, dispatcher.clone(), None, tx);
        session.send_user_message("please echo").await.unwrap();

        let events = collect(&mut rx);
        let kinds: Vec<&'static str> = events
            .iter()
            .map(|e| match e {
                AgentEvent::AssistantTextDelta { .. } => "delta",
                AgentEvent::ToolCallStart { .. } => "tool_start",
                AgentEvent::ToolCallEnd { .. } => "tool_end",
                AgentEvent::TurnEnd { more_turns, .. } => {
                    if *more_turns {
                        "turn_end_continue"
                    } else {
                        "turn_end_final"
                    }
                }
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["tool_start", "tool_end", "turn_end_continue", "delta", "turn_end_final"]
        );
        // dispatcher saw the call
        let calls = dispatcher.last_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "echo");

        // conversation: user + assistant(tool_use) + user(tool_result) + assistant(text)
        assert_eq!(session.messages.len(), 4);
    }

    #[tokio::test]
    async fn turn_end_carries_per_iteration_usage() {
        // Two-iteration run: tool turn reports 100/20, final turn 250/80.
        // Each TurnEnd must carry ITS OWN stream's usage so a consumer summing
        // across events gets 350 in / 100 out with no double counting.
        let provider = Arc::new(ScriptedProvider::new(vec![
            vec![
                ProviderEvent::ToolCall {
                    id: "tc_1".into(),
                    name: "echo".into(),
                    input: serde_json::json!({}),
                },
                ProviderEvent::ToolResultRequested,
                ProviderEvent::Usage { input_tokens: 100, output_tokens: 20 },
                ProviderEvent::Stop { reason: StopReason::ToolUse },
            ],
            vec![
                ProviderEvent::Text("done".into()),
                ProviderEvent::Usage { input_tokens: 250, output_tokens: 80 },
                ProviderEvent::Stop { reason: StopReason::EndTurn },
            ],
        ]));
        let dispatcher = Arc::new(ScriptedDispatcher {
            defs: vec![ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            outcomes: Mutex::new(vec![ToolDispatchOutcome::ok("ok")]),
            last_calls: Mutex::new(vec![]),
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut session = AgentSession::new(provider, dispatcher, None, tx);
        session.send_user_message("go").await.unwrap();

        let usages: Vec<TokenUsage> = collect(&mut rx)
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::TurnEnd { usage, .. } => Some(usage),
                _ => None,
            })
            .collect();
        assert_eq!(usages.len(), 2);
        assert_eq!((usages[0].input_tokens, usages[0].output_tokens), (100, 20));
        assert_eq!((usages[1].input_tokens, usages[1].output_tokens), (250, 80));
        let total_in: u64 = usages.iter().map(|u| u.input_tokens).sum();
        let total_out: u64 = usages.iter().map(|u| u.output_tokens).sum();
        assert_eq!((total_in, total_out), (350, 100));
    }

    #[tokio::test]
    async fn loop_safety_cap_returns_error_after_max_iterations() {
        let plan: Vec<Vec<ProviderEvent>> = (0..=MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    ProviderEvent::ToolCall {
                        id: format!("tc_{i}"),
                        name: "echo".into(),
                        input: Value::Null,
                    },
                    ProviderEvent::ToolResultRequested,
                    ProviderEvent::Stop { reason: StopReason::ToolUse },
                ]
            })
            .collect();
        let provider = Arc::new(ScriptedProvider::new(plan));
        let outcomes = (0..=MAX_TURN_ITERATIONS)
            .map(|_| ToolDispatchOutcome::ok("noop"))
            .collect();
        let dispatcher = Arc::new(ScriptedDispatcher {
            defs: vec![ToolDef {
                name: "echo".into(),
                description: "x".into(),
                input_schema: Value::Null,
            }],
            outcomes: Mutex::new(outcomes),
            last_calls: Mutex::new(vec![]),
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut session = AgentSession::new(provider, dispatcher, None, tx);
        let err = session.send_user_message("loop").await.unwrap_err();
        assert!(matches!(err, AgentError::LoopLimitExceeded));
    }
}
