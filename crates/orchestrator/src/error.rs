//! Typed errors surfaced across the worker boundary.

use std::path::PathBuf;

use thiserror::Error;

/// Anything that can go wrong when invoking a Python worker.
#[derive(Debug, Error)]
pub enum WorkerError {
    /// Failed to launch the configured Python interpreter (binary missing, permission denied, ...).
    #[error("failed to spawn Python interpreter `{program}` in `{cwd}`: {source}")]
    Spawn {
        program: String,
        cwd: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Worker process exited before producing a response. The captured stderr is
    /// preserved verbatim so the user can see Python tracebacks unchanged.
    #[error("worker exited with status {status} before responding\nstderr:\n{stderr}")]
    EarlyExit { status: String, stderr: String },

    /// I/O failed while reading from or writing to the worker's stdio.
    #[error("io error while talking to worker: {0}")]
    Io(#[from] std::io::Error),

    /// The worker produced bytes we could not parse as JSON-RPC.
    #[error("malformed response from worker: {message}\nraw: {raw}")]
    MalformedResponse { message: String, raw: String },

    /// The worker returned a structured JSON-RPC error.
    #[error("worker returned RPC error {code}: {message}")]
    Rpc { code: i64, message: String },

    /// The worker did not respond within the configured timeout. Worker is killed before this returns.
    #[error("worker timed out after {seconds}s for method `{method}`")]
    Timeout { method: String, seconds: u64 },

    /// `Stdio::piped()` did not yield a stream handle. Indicates a `tokio::process` bug or OS
    /// resource exhaustion; in practice this is unreachable for our configuration.
    #[error("worker stdio handle missing ({stream}); this indicates a tokio/OS regression")]
    StdioMissing { stream: &'static str },
}
