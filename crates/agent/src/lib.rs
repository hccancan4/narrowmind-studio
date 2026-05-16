//! `NarrowMind Studio` agent loop.
//!
//! Hosts the `Provider` trait, the typed event stream every adapter produces, and the agent
//! loop that drives the conversation. Adapters live under [`provider`] (Anthropic in v1;
//! `OpenAI` / `Ollama` / custom land in Phase 7).
//!
//! The trait deliberately speaks in [`ProviderEvent`] rather than vendor-specific content blocks
//! so that swapping providers is a config change rather than a refactor.

pub mod anthropic;
pub mod error;
pub mod provider;
pub mod types;

pub use anthropic::AnthropicProvider;
pub use error::{ProviderError, ProviderResult};
pub use provider::Provider;
pub use types::{Message, MessageContent, ProviderEvent, Role, StopReason, ToolDef};

/// Crate version string for build-info reporting.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
