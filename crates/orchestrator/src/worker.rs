//! Single-shot JSON-RPC 2.0 client that spawns a worker, sends one request, and waits for one reply.
//!
//! Phase 0 only needs this much: enough to round-trip the `hello` method through a real Python
//! subprocess and prove the Rust ↔ Python boundary works. Long-lived multiplexed workers, request
//! pipelining, and progress notifications land in later phases — keeping the surface tiny now lets
//! us replace it without churning callers.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, warn};

use crate::error::WorkerError;

/// Default timeout for one-shot worker calls. The `hello` round-trip cold-starts a Python
/// interpreter and imports the worker package, which on a warm cache takes <1 s but can stretch
/// past 10 s on the first run of a fresh venv. Twenty seconds is a forgiving Phase 0 ceiling;
/// real workers will set their own deadlines.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// How to launch a Python interpreter for a worker. The defaults assume the repo's `uv` workspace
/// from the workspace root, which is the only configuration that exists during Phase 0 dev.
#[derive(Debug, Clone)]
pub struct PythonRunner {
    /// Program to execute (e.g. `uv`, `python`, or a venv-absolute interpreter path).
    pub program: String,
    /// Leading arguments before the worker module (e.g. `["run", "python"]` for `uv`).
    pub leading_args: Vec<String>,
    /// Working directory the program is launched in. uv resolves the venv from cwd, so this must
    /// point at the workspace root containing `pyproject.toml`.
    pub cwd: PathBuf,
}

impl PythonRunner {
    /// Construct the default runner: `uv run python` from the given workspace root.
    #[must_use]
    pub fn uv_workspace(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            program: "uv".to_string(),
            leading_args: vec!["run".into(), "python".into()],
            cwd: workspace_root.into(),
        }
    }
}

/// A single JSON-RPC call to a worker module.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    /// Name of the worker module to launch (`python -m <module>`).
    pub module: String,
    /// JSON-RPC method to invoke on the worker.
    pub method: String,
    /// JSON params object passed to the method.
    pub params: Value,
    /// Optional override for the default timeout.
    pub timeout: Option<Duration>,
}

impl WorkerCommand {
    /// Convenience constructor for the `hello` round-trip used by Phase 0.
    #[must_use]
    pub fn hello(name: Option<&str>) -> Self {
        let params = match name {
            Some(n) => serde_json::json!({ "name": n }),
            None => serde_json::json!({}),
        };
        Self {
            module: "narrowmind_workers".to_string(),
            method: "hello".to_string(),
            params,
            timeout: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Value,
    result: Option<Value>,
    error: Option<RpcErrorBody>,
}

#[derive(Debug, Deserialize)]
struct RpcErrorBody {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: &'a Value,
}

/// Spawn a worker subprocess, perform one request/response cycle, and return the result value.
///
/// On any failure path the worker process is killed before we return; we never leak a child.
pub async fn call_worker(runner: &PythonRunner, cmd: &WorkerCommand) -> Result<Value, WorkerError> {
    let timeout = cmd.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let mut child = build_command(runner, &cmd.module)
        .spawn()
        .map_err(|source| WorkerError::Spawn {
            program: runner.program.clone(),
            cwd: runner.cwd.clone(),
            source,
        })?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or(WorkerError::StdioMissing { stream: "stdin" })?;
    let stdout = child
        .stdout
        .take()
        .ok_or(WorkerError::StdioMissing { stream: "stdout" })?;
    let stderr = child
        .stderr
        .take()
        .ok_or(WorkerError::StdioMissing { stream: "stderr" })?;

    let request = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: &cmd.method,
        params: &cmd.params,
    };
    let mut payload = serde_json::to_vec(&request).map_err(|e| WorkerError::MalformedResponse {
        message: format!("could not serialize request: {e}"),
        raw: String::new(),
    })?;
    payload.push(b'\n');

    let result = tokio::time::timeout(timeout, async {
        stdin.write_all(&payload).await?;
        stdin.shutdown().await?;
        drop(stdin);

        let mut reader = BufReader::new(stdout);
        let mut first_line = String::new();
        let n = reader.read_line(&mut first_line).await?;
        if n == 0 {
            return read_early_exit(&mut child, stderr).await;
        }

        debug!(line = %first_line.trim_end(), "worker response");
        let parsed: RpcResponse =
            serde_json::from_str(first_line.trim_end()).map_err(|e| WorkerError::MalformedResponse {
                message: e.to_string(),
                raw: first_line.clone(),
            })?;

        if let Some(err) = parsed.error {
            return Err(WorkerError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        parsed.result.ok_or_else(|| WorkerError::MalformedResponse {
            message: "response had neither result nor error".into(),
            raw: first_line,
        })
    })
    .await;

    let Ok(outcome) = result else {
        warn!(method = cmd.method, "worker timed out, killing child");
        // best-effort: kill the child so we never leak a process
        let _ = child.start_kill();
        return Err(WorkerError::Timeout {
            method: cmd.method.clone(),
            seconds: timeout.as_secs(),
        });
    };

    // We don't wait on the child here; on success it will EOF and exit shortly. Dropping the
    // tokio::process::Child sends SIGKILL on drop when configured, but the default is to detach,
    // which is the right behavior — Python should observe stdin EOF and exit cleanly.
    let _ = child.wait().await;
    outcome
}

fn build_command(runner: &PythonRunner, module: &str) -> Command {
    let mut cmd = Command::new(&runner.program);
    cmd.args(&runner.leading_args)
        .arg("-m")
        .arg(module)
        .current_dir(workspace_or_self(&runner.cwd))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

fn workspace_or_self(cwd: &Path) -> PathBuf {
    if cwd.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        cwd.to_path_buf()
    }
}

async fn read_early_exit(
    child: &mut tokio::process::Child,
    stderr: tokio::process::ChildStderr,
) -> Result<Value, WorkerError> {
    let status = child.wait().await?;
    let mut buf = String::new();
    let mut reader = BufReader::new(stderr);
    let _ = reader.read_to_string(&mut buf).await;
    Err(WorkerError::EarlyExit {
        status: format!("{status}"),
        stderr: buf,
    })
}

// re-export so callers don't need a tokio::io import
use tokio::io::AsyncReadExt as _;
