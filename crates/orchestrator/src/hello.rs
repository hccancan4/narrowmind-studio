//! High-level `hello` round-trip used by the Tauri debug button.

use serde::{Deserialize, Serialize};

use crate::error::WorkerError;
use crate::worker::{call_worker, PythonRunner, WorkerCommand};

/// Strongly-typed payload returned by the `hello` worker method. Mirrors
/// `workers/py/narrowmind_workers/hello.py`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub message: String,
    pub worker_version: String,
    pub worker_pid: i64,
    pub python_version: String,
    pub platform: String,
}

/// Spawn the Python worker once, invoke `hello`, and parse the response.
///
/// `name` is forwarded to the worker; pass `None` to use the worker's default of `"world"`.
pub async fn hello_round_trip(
    runner: &PythonRunner,
    name: Option<&str>,
) -> Result<HelloResult, WorkerError> {
    let cmd = WorkerCommand::hello(name);
    let value = call_worker(runner, &cmd).await?;
    serde_json::from_value(value.clone()).map_err(|e| WorkerError::MalformedResponse {
        message: format!("hello result did not match HelloResult schema: {e}"),
        raw: value.to_string(),
    })
}
