//! The `Provider` trait. Adapters implement it for a specific vendor; the agent loop only
//! ever sees the trait object, so adding `OpenAI` / `Ollama` / custom endpoints in Phase 7 is
//! a new module rather than a refactor of the loop.

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::error::ProviderResult;
use crate::types::{Message, ProviderEvent, ToolDef};

/// Streaming LLM provider. Implementations must be `Send + Sync` so a single
/// `Box<dyn Provider>` can be shared across the orchestrator's async tasks.
///
/// The stream item type is bare [`ProviderEvent`] — fatal stream-level failures are signalled
/// by emitting `ProviderEvent::Stop { reason: StopReason::Error(_) }` and then closing the
/// stream, so consumers never need a per-item `Result`. Setup errors (auth, malformed request)
/// surface from `stream(...)` itself before any events are produced.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier the UI shows in settings (e.g. `"anthropic"`).
    fn name(&self) -> &'static str;

    /// Open a streaming response for the given conversation and tool set.
    ///
    /// `messages` may include a `Role::System` entry; adapters map it into whatever their API
    /// expects. `tools` is empty when the model is being run without function calling.
    async fn stream(
        &self,
        messages: Vec<Message>,
        tools: &[ToolDef],
    ) -> ProviderResult<BoxStream<'static, ProviderEvent>>;
}
