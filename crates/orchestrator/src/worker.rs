//! Single-shot JSON-RPC 2.0 client that spawns a worker, sends one request, and waits for one reply.
//!
//! Phase 0 only needs this much: enough to round-trip the `hello` method through a real Python
//! subprocess and prove the Rust ↔ Python boundary works. Long-lived multiplexed workers, request
//! pipelining, and progress notifications land in later phases — keeping the surface tiny now lets
//! us replace it without churning callers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::WorkerError;
use crate::retry::timeouts;

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
    /// Environment variables to set on the spawned process. Merged with inherited env; if a key
    /// here already exists in the parent env, this value wins. Used by the orchestrator to point
    /// `HF_HOME` at the studio's own cache directory instead of `~/.cache/huggingface`.
    pub envs: BTreeMap<String, String>,
}

impl PythonRunner {
    /// Construct the default runner: `uv run python` from the given workspace root.
    #[must_use]
    pub fn uv_workspace(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            program: "uv".to_string(),
            leading_args: vec!["run".into(), "python".into()],
            cwd: workspace_root.into(),
            envs: BTreeMap::new(),
        }
    }

    /// Builder: add or override one environment variable on the spawned process.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.insert(key.into(), value.into());
        self
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
    let timeout = cmd.timeout.unwrap_or(timeouts::WORKER_DEFAULT);
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

/// Shared between the one-shot path here and the long-lived `WorkerPool`
/// (`crate::worker_pool`) — both spawn `<program> <leading_args> -m <module>`
/// with identical stdio piping and env merging, so the spawn recipe must live
/// in exactly one place.
pub(crate) fn build_command(runner: &PythonRunner, module: &str) -> Command {
    let mut cmd = Command::new(&runner.program);
    cmd.args(&runner.leading_args)
        .arg("-m")
        .arg(module)
        .current_dir(workspace_or_self(&runner.cwd))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Apply explicit env overrides last so they win over inherited values. Used by Phase 2
    // ingestion to point HF_HOME / HF_HUB_CACHE / TRANSFORMERS_CACHE at the studio's cache.
    for (k, v) in &runner.envs {
        cmd.env(k, v);
    }
    cmd
}

fn workspace_or_self(cwd: &Path) -> PathBuf {
    if cwd.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        cwd.to_path_buf()
    }
}

// ---------------------------------------------------------------------------
// One-shot WITH streaming notifications (Phase 4 — training metrics)
// ---------------------------------------------------------------------------

/// One-shot worker call that surfaces JSON-RPC notifications mid-flight.
///
/// Same lifecycle as [`call_worker`] — spawn, one request, one response,
/// child exits; kill = clean cancel; **no retry** — but the read phase is the
/// pool's id-matching loop: id-less frames (`{"jsonrpc","method","params"}`)
/// are dispatched to `on_notify(method, params)` instead of being treated as
/// protocol errors. Built for hours-long training runs that stream a
/// `training.metric` notification every step.
///
/// Timeout semantics are **activity-based**, not total-deadline: `idle_timeout`
/// measures *silence*. Every frame (notification or response) refreshes the
/// timer, so a 3-hour run with steady step events needs no absurd static
/// ceiling, while a genuinely wedged worker is reaped after one quiet window.
///
/// `cancel` is a `watch::Receiver<bool>`: flip the sender to `true` to kill
/// the child and return [`WorkerError::Cancelled`]. Progress already streamed
/// stays valid (the training worker persists `metrics.jsonl` itself).
pub async fn call_worker_streaming(
    runner: &PythonRunner,
    cmd: &WorkerCommand,
    idle_timeout: Duration,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    mut on_notify: impl FnMut(&str, &Value) + Send,
) -> Result<Value, WorkerError> {
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

    // Drain stderr continuously (same rationale as the pool: an undrained
    // pipe blocks a chatty child after ~64 KB, which reads as a hang).
    // Keep a bounded tail for traceback attachment on failure.
    let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::<String>::with_capacity(100),
    ));
    let tail_for_task = stderr_tail.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            debug!(target: "worker_stderr", "{line}");
            let mut tail = tail_for_task.lock().expect("stderr tail poisoned");
            if tail.len() == 100 {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    });
    let tail_snapshot = |t: &std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>| {
        t.lock()
            .map(|d| d.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    };

    const REQUEST_ID: u64 = 1;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "method": cmd.method,
        "params": cmd.params,
    });
    let mut payload = serde_json::to_vec(&request).map_err(|e| WorkerError::MalformedResponse {
        message: format!("could not serialize request: {e}"),
        raw: String::new(),
    })?;
    payload.push(b'\n');
    stdin.write_all(&payload).await?;
    stdin.flush().await?;
    // Keep stdin OPEN: closing it would make serve_forever exit after this
    // request, which is fine for the result but would race the final frames.
    // The child exits when we drop stdin after the loop (or on kill).

    let mut reader = BufReader::new(stdout);
    let kill = |child: &mut tokio::process::Child| {
        let _ = child.start_kill();
    };

    // When the cancel sender is dropped (caller doesn't care about cancel),
    // `changed()` resolves with Err instantly and forever — gate the branch
    // off after the first Err so the select! doesn't busy-loop.
    let mut cancel_open = true;

    loop {
        let mut line = String::new();
        let n = tokio::select! {
            read = reader.read_line(&mut line) => match read {
                Ok(n) => n,
                Err(e) => {
                    kill(&mut child);
                    let _ = child.wait().await;
                    stderr_task.abort();
                    return Err(WorkerError::Io(e));
                }
            },
            _ = tokio::time::sleep(idle_timeout) => {
                warn!(method = %cmd.method, idle_secs = idle_timeout.as_secs(), "streaming worker idle past threshold; killing");
                kill(&mut child);
                let _ = child.wait().await;
                stderr_task.abort();
                return Err(WorkerError::Timeout {
                    method: cmd.method.clone(),
                    seconds: idle_timeout.as_secs(),
                });
            }
            changed = cancel.changed(), if cancel_open => {
                match changed {
                    Ok(()) if *cancel.borrow() => {
                        info!(method = %cmd.method, "streaming worker call cancelled by caller");
                        kill(&mut child);
                        let _ = child.wait().await;
                        stderr_task.abort();
                        return Err(WorkerError::Cancelled { method: cmd.method.clone() });
                    }
                    Ok(()) => continue,          // spurious false flip — ignore
                    Err(_) => { cancel_open = false; continue } // sender dropped
                }
            }
        };

        if n == 0 {
            // EOF before the response: the worker died mid-run.
            let status = child.wait().await.map(|s| format!("{s}")).unwrap_or_else(|e| e.to_string());
            stderr_task.abort();
            return Err(WorkerError::EarlyExit {
                status,
                stderr: tail_snapshot(&stderr_tail),
            });
        }

        let trimmed = line.trim_end();
        let frame: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                kill(&mut child);
                let _ = child.wait().await;
                stderr_task.abort();
                return Err(WorkerError::MalformedResponse {
                    message: e.to_string(),
                    raw: trimmed.to_string(),
                });
            }
        };

        // Notification: id-less frame with a method — stream to the caller
        // and refresh the idle window (the select! loop restarts the sleep).
        if frame.get("id").is_none() {
            if let Some(method) = frame.get("method").and_then(Value::as_str) {
                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                on_notify(method, &params);
            } else {
                debug!(raw = %trimmed, "skipping id-less frame without method");
            }
            continue;
        }

        // Response frame: must match our request id.
        if frame.get("id").and_then(Value::as_u64) != Some(REQUEST_ID) {
            debug!(raw = %trimmed, "skipping frame with unexpected id");
            continue;
        }

        drop(stdin); // EOF → serve_forever ends → child exits cleanly
        let _ = child.wait().await;
        stderr_task.abort();

        if let Some(err) = frame.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown rpc error")
                .to_string();
            return Err(WorkerError::Rpc { code, message });
        }
        return frame
            .get("result")
            .cloned()
            .ok_or_else(|| WorkerError::MalformedResponse {
                message: "response had neither result nor error".into(),
                raw: trimmed.to_string(),
            });
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
