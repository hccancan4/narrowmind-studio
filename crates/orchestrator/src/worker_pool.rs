//! Long-lived JSON-RPC worker pool: one persistent Python child per worker module.
//!
//! Phase 0 shipped a one-shot model (`crate::worker::call_worker`) — spawn, one
//! request, one response, child exits. That was fine until Phase 3 put BGE-small
//! behind `rag.query`: every retrieval paid ~5-8 s of interpreter start +
//! sentence-transformers import + model load, on every single call, and
//! `run_eval parallelism=4` ran four simultaneous copies of the model in RAM.
//!
//! The pool keeps one child per module alive across calls so in-process model
//! caches (`lru_cache` singletons in the Python workers) actually pay off.
//! Design decisions, in order of importance:
//!
//! - **Calls are serialized per worker** behind an async `Mutex`, not
//!   multiplexed. The Python server (`rpc/server.py::serve_forever`) is
//!   single-threaded and strictly sequential, so request-id multiplexing would
//!   only move the queue into the OS pipe buffer. Warm `rag.query` is
//!   50-300 ms; eval's wall-clock is dominated by llama.cpp + judge calls.
//! - **Timeouts are deadlines over the whole call** (queue wait + roundtrip).
//!   A caller that times out while *queued* leaves the worker untouched. A
//!   caller that times out *in flight* kills the child (state unknown) and the
//!   next caller respawns lazily. This preserves the one-shot model's "a hung
//!   worker can never wedge the app" property.
//! - **Dead children are respawned transparently, once.** Write failure or
//!   read EOF → kill, clear, respawn, retry the same request a single time.
//!   Timeouts are never retried (the handler may have partially executed).
//! - **stderr is drained continuously.** A long-lived child that logs to an
//!   undrained pipe blocks after ~64 KB, which presents as a mystery hang.
//!   The drain task forwards lines to `tracing` and keeps a bounded tail so
//!   failure paths can still attach Python tracebacks.
//! - **Scope: fast repeated queries only.** Batch jobs (embed_chunks 900 s,
//!   ingestion 600 s, model downloads 60 min) stay on `call_worker` — pooling
//!   them would head-of-line-block Local Chat's retrieval, and per-job crash
//!   isolation is a feature there, not a cost.
//!
//! Forward-compat note (Phase 4): because calls are serialized, any id-less
//! frame that arrives while a request is in flight unambiguously belongs to
//! that request — the read loop already skips such notification frames, so
//! streaming progress (training logs etc.) can be added as a callback without
//! redesigning the protocol.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::error::WorkerError;
use crate::worker::{build_command, PythonRunner, WorkerCommand};

/// Default per-call deadline when the command doesn't specify one. Matches the
/// one-shot default: forgiving enough for a cold spawn + model load on first
/// call, tight enough that a wedged worker is reaped quickly.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// How many trailing stderr lines to keep per worker for error attachment.
const STDERR_TAIL_LINES: usize = 100;

/// Grace period between closing stdin (EOF → Python exits its serve loop) and
/// hard-killing the process tree during shutdown.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Snapshot of one pooled worker for debugging / future UI surfacing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PooledWorkerStatus {
    pub module: String,
    pub pid: Option<u32>,
    pub calls_served: u64,
}

/// One persistent child per worker module, spawned lazily, calls serialized.
/// Cheap to clone-share via `Arc`; the desktop app owns one in `AppState`.
pub struct WorkerPool {
    runner: Arc<PythonRunner>,
    workers: Mutex<HashMap<String, Arc<PersistentWorker>>>,
}

struct PersistentWorker {
    module: String,
    /// `None` = not spawned yet, or cleared after a kill. The mutex is the
    /// serialization point: exactly one caller talks to the child at a time.
    slot: Mutex<Option<LiveChild>>,
}

struct LiveChild {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    calls_served: u64,
    stderr_tail: Arc<std::sync::Mutex<VecDeque<String>>>,
    stderr_task: tokio::task::JoinHandle<()>,
}

/// Internal classification for one roundtrip attempt, so `call()` can decide
/// retry vs. kill vs. surface without string-matching on `WorkerError`.
enum AttemptError {
    /// Write failed or read hit EOF — the child is gone. Retryable once.
    DeadChild(WorkerError),
    /// Deadline elapsed mid-flight. Kill, never retry.
    Timeout,
    /// Application-level outcome (RPC error, malformed frame). Child stays up.
    Other(WorkerError),
}

impl WorkerPool {
    #[must_use]
    pub fn new(runner: Arc<PythonRunner>) -> Self {
        Self {
            runner,
            workers: Mutex::new(HashMap::new()),
        }
    }

    /// JSON-RPC call against the persistent worker for `cmd.module`. Spawns
    /// lazily on first use; respawns transparently (once) if the child died.
    /// `cmd.timeout` is a total deadline covering queue wait + roundtrip.
    pub async fn call(&self, cmd: &WorkerCommand) -> Result<Value, WorkerError> {
        let worker = self.worker_for(&cmd.module).await;
        let timeout = cmd.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let deadline = Instant::now() + timeout;

        // Queue phase: waiting for the worker mutex counts against the
        // deadline, but timing out here must NOT touch the worker — some other
        // caller legitimately owns it.
        let Ok(mut slot) = tokio::time::timeout_at(deadline, worker.slot.lock()).await else {
            return Err(WorkerError::Timeout {
                method: cmd.method.clone(),
                seconds: timeout.as_secs(),
            });
        };

        for attempt in 0..2 {
            // (Re)spawn if the slot is empty or the child silently exited
            // while idle — `try_wait` catches the common died-between-calls
            // case before we waste a write on a broken pipe.
            self.ensure_live(&mut slot, &cmd.module)?;
            let live = slot.as_mut().expect("ensure_live populated the slot");

            match Self::roundtrip(live, cmd, deadline).await {
                Ok(value) => {
                    live.calls_served += 1;
                    return Ok(value);
                }
                Err(AttemptError::DeadChild(e)) => {
                    let tail = stderr_tail_snapshot(live);
                    kill_and_clear(&mut slot).await;
                    if attempt == 0 {
                        info!(module = %cmd.module, "pooled worker died; respawning and retrying once");
                        continue;
                    }
                    // Second failure in a row: surface with traceback context.
                    return Err(attach_stderr(e, &tail));
                }
                Err(AttemptError::Timeout) => {
                    // In-flight timeout: the worker's state is unknown — it may
                    // be wedged or mid-write. Kill it so the next caller gets a
                    // fresh child, and do NOT retry (partial execution risk).
                    warn!(module = %cmd.module, method = %cmd.method, "pooled worker call timed out in flight; killing child");
                    kill_and_clear(&mut slot).await;
                    return Err(WorkerError::Timeout {
                        method: cmd.method.clone(),
                        seconds: timeout.as_secs(),
                    });
                }
                Err(AttemptError::Other(e)) => return Err(e),
            }
        }
        unreachable!("attempt loop returns on every branch by the second pass")
    }

    /// Kill all children: graceful stdin-EOF first (Python's serve loop exits
    /// on EOF), short wait, then hard tree-kill. Idempotent.
    pub async fn shutdown(&self) {
        let workers: Vec<Arc<PersistentWorker>> =
            self.workers.lock().await.values().cloned().collect();
        for worker in workers {
            let mut slot = worker.slot.lock().await;
            if slot.is_some() {
                info!(module = %worker.module, "shutting down pooled worker");
            }
            graceful_stop(&mut slot).await;
        }
    }

    /// Kill one module's child (manual recovery; future idle-TTL hook).
    pub async fn stop(&self, module: &str) {
        let worker = { self.workers.lock().await.get(module).cloned() };
        if let Some(worker) = worker {
            graceful_stop(&mut *worker.slot.lock().await).await;
        }
    }

    /// Debug snapshot of live workers. Busy workers (mutex held by an in-flight
    /// call) are reported with `pid: None` rather than blocking on them.
    pub async fn status(&self) -> Vec<PooledWorkerStatus> {
        let workers: Vec<Arc<PersistentWorker>> =
            self.workers.lock().await.values().cloned().collect();
        let mut out = Vec::with_capacity(workers.len());
        for worker in workers {
            let (pid, calls_served) = match worker.slot.try_lock() {
                Ok(slot) => match slot.as_ref() {
                    Some(live) => (live.child.id(), live.calls_served),
                    None => (None, 0),
                },
                Err(_) => (None, 0), // busy — don't block a live call for telemetry
            };
            out.push(PooledWorkerStatus {
                module: worker.module.clone(),
                pid,
                calls_served,
            });
        }
        out
    }

    async fn worker_for(&self, module: &str) -> Arc<PersistentWorker> {
        let mut map = self.workers.lock().await;
        map.entry(module.to_string())
            .or_insert_with(|| {
                Arc::new(PersistentWorker {
                    module: module.to_string(),
                    slot: Mutex::new(None),
                })
            })
            .clone()
    }

    fn ensure_live(
        &self,
        slot: &mut Option<LiveChild>,
        module: &str,
    ) -> Result<(), WorkerError> {
        // Drop a child that exited while idle so we respawn below.
        if let Some(live) = slot.as_mut() {
            if let Ok(Some(status)) = live.child.try_wait() {
                let tail = stderr_tail_snapshot(live);
                warn!(module, %status, stderr_tail = %tail, "pooled worker exited while idle; respawning");
                *slot = None;
            }
        }
        if slot.is_none() {
            *slot = Some(self.spawn(module)?);
        }
        Ok(())
    }

    fn spawn(&self, module: &str) -> Result<LiveChild, WorkerError> {
        let mut child =
            build_command(&self.runner, module)
                .spawn()
                .map_err(|source| WorkerError::Spawn {
                    program: self.runner.program.clone(),
                    cwd: self.runner.cwd.clone(),
                    source,
                })?;
        let stdin = child
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

        // Drain stderr forever. Without this, a chatty child fills the ~64 KB
        // pipe buffer and blocks inside a print() — which looks like a hang on
        // our side and gets a healthy worker killed by the timeout path.
        let stderr_tail: Arc<std::sync::Mutex<VecDeque<String>>> =
            Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(
                STDERR_TAIL_LINES,
            )));
        let tail_for_task = stderr_tail.clone();
        let module_owned = module.to_string();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                debug!(target: "worker_stderr", module = %module_owned, "{line}");
                let mut tail = tail_for_task.lock().expect("stderr tail poisoned");
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });

        info!(module, pid = child.id(), "spawned pooled worker");
        Ok(LiveChild {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            calls_served: 0,
            stderr_tail,
            stderr_task,
        })
    }

    /// One request/response cycle on a live child. Skips notification frames
    /// (id-less or non-matching id) until the response for our request id
    /// arrives — the Phase 4 hook point for progress streaming.
    async fn roundtrip(
        live: &mut LiveChild,
        cmd: &WorkerCommand,
        deadline: Instant,
    ) -> Result<Value, AttemptError> {
        let id = live.next_id;
        live.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": cmd.method,
            "params": cmd.params,
        });
        let mut payload = serde_json::to_vec(&request).map_err(|e| {
            AttemptError::Other(WorkerError::MalformedResponse {
                message: format!("could not serialize request: {e}"),
                raw: String::new(),
            })
        })?;
        payload.push(b'\n');

        // Write phase. A broken pipe here means the child is gone.
        match tokio::time::timeout_at(deadline, async {
            live.stdin.write_all(&payload).await?;
            live.stdin.flush().await
        })
        .await
        {
            Err(_) => return Err(AttemptError::Timeout),
            Ok(Err(e)) => return Err(AttemptError::DeadChild(WorkerError::Io(e))),
            Ok(Ok(())) => {}
        }

        // Read phase: loop until the line whose `id` matches ours.
        loop {
            let mut line = String::new();
            let n = match tokio::time::timeout_at(deadline, live.stdout.read_line(&mut line)).await
            {
                Err(_) => return Err(AttemptError::Timeout),
                Ok(Err(e)) => return Err(AttemptError::DeadChild(WorkerError::Io(e))),
                Ok(Ok(n)) => n,
            };
            if n == 0 {
                // EOF after a successful write: the handler crashed the process.
                return Err(AttemptError::DeadChild(WorkerError::EarlyExit {
                    status: "eof mid-call".into(),
                    stderr: String::new(), // caller attaches the drained tail
                }));
            }
            let trimmed = line.trim_end();
            debug!(line = %trimmed, "pooled worker frame");

            let frame: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    return Err(AttemptError::Other(WorkerError::MalformedResponse {
                        message: e.to_string(),
                        raw: trimmed.to_string(),
                    }));
                }
            };

            // Notification (no id) or stale frame (different id): skip. With
            // serialized calls a notification here belongs to OUR in-flight
            // request — Phase 4 will surface these as ToolEvent::Progress.
            let frame_id = frame.get("id").and_then(Value::as_u64);
            if frame_id != Some(id) {
                debug!(?frame_id, expected = id, "skipping non-response frame");
                continue;
            }

            if let Some(err) = frame.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown rpc error")
                    .to_string();
                return Err(AttemptError::Other(WorkerError::Rpc { code, message }));
            }
            return frame
                .get("result")
                .cloned()
                .ok_or_else(|| {
                    AttemptError::Other(WorkerError::MalformedResponse {
                        message: "response had neither result nor error".into(),
                        raw: trimmed.to_string(),
                    })
                });
        }
    }
}

fn stderr_tail_snapshot(live: &LiveChild) -> String {
    live.stderr_tail
        .lock()
        .map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

/// Fold the drained stderr tail into an error so Python tracebacks survive the
/// pool the same way they do in the one-shot path's `EarlyExit`.
fn attach_stderr(e: WorkerError, tail: &str) -> WorkerError {
    if tail.is_empty() {
        return e;
    }
    match e {
        WorkerError::EarlyExit { status, .. } => WorkerError::EarlyExit {
            status,
            stderr: tail.to_string(),
        },
        other => WorkerError::EarlyExit {
            status: format!("worker died: {other}"),
            stderr: tail.to_string(),
        },
    }
}

/// Hard-kill the child and clear the slot. On Windows the direct child is the
/// `uv` shim, so `start_kill` alone can orphan the python grandchild — use
/// `taskkill /T` to fell the whole tree first, best-effort.
async fn kill_and_clear(slot: &mut Option<LiveChild>) {
    if let Some(mut live) = slot.take() {
        kill_tree(&mut live.child).await;
        live.stderr_task.abort();
    }
}

/// Graceful stop: close stdin so Python's `for line in stdin` loop ends and the
/// process exits cleanly; only escalate to a tree kill after a short grace.
async fn graceful_stop(slot: &mut Option<LiveChild>) {
    if let Some(mut live) = slot.take() {
        drop(live.stdin); // EOF → serve_forever returns → clean exit
        match tokio::time::timeout(SHUTDOWN_GRACE, live.child.wait()).await {
            Ok(_) => {}
            Err(_) => kill_tree(&mut live.child).await,
        }
        live.stderr_task.abort();
    }
}

async fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        // /T = tree (uv → python), /F = force. Best-effort: taskkill may be
        // missing on stripped-down systems; start_kill below still reaps the
        // direct child, and stdin EOF eventually unblocks a healthy python.
        let _ = tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}
