//! Typed errors surfaced by `Provider` adapters.

use thiserror::Error;

/// Result alias used across provider adapters.
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

#[derive(Debug, Error)]
pub enum ProviderError {
    /// API key, base URL, or model id was missing or empty.
    #[error("provider misconfigured: {0}")]
    Config(String),

    /// HTTP transport failure (timeout, DNS, TLS, ...).
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// Provider returned a non-2xx HTTP status. `body` is the raw response (best-effort UTF-8).
    #[error("provider returned status {status}: {body}")]
    Status { status: u16, body: String },

    /// Provider sent a payload we could not parse.
    #[error("malformed provider response: {0}")]
    Malformed(String),

    /// Streaming connection ended unexpectedly (closed mid-event, etc.).
    #[error("stream ended unexpectedly: {0}")]
    StreamClosed(String),
}
