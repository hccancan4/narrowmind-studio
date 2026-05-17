//! Long-lived inference server lifecycle.
//!
//! Per Phase 3 decision F: one server per app instance (8 GB VRAM only fits one 7B
//! `Q4_K_M` model at a time). Started lazily when the first chat / `rag_chat` tool fires,
//! killed on app shutdown, on project switch, or after 10 min of idleness.
//!
//! The server is `llama_cpp.server` (OpenAI-compatible HTTP). Rust owns the
//! `tokio::process::Child` plus a "last touched" timestamp for the TTL thread.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::worker::{call_worker, PythonRunner, WorkerCommand};

/// Default port we try first. If already in use we increment until something works.
pub const DEFAULT_PORT: u16 = 8765;
/// Maximum port-probe increments before giving up.
const PORT_PROBE_LIMIT: u16 = 50;
/// How long we wait for `/health` to come up after spawning the server.
const STARTUP_HEALTH_TIMEOUT: Duration = Duration::from_secs(120);
/// Idle TTL — after this much time with no `mark_used()`, the server is killed.
pub const IDLE_TTL: Duration = Duration::from_secs(10 * 60);

/// Where to pull the GGUF from. Defaults from Phase 3 decision A (Qwen2.5-7B `Q4_K_M`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSpec {
    pub repo_id: String,
    pub filename: String,
    /// Context window for the server. 4096 is a safe default for 7B `Q4_K_M` on 8 GB.
    pub n_ctx: u32,
    /// llama.cpp `-ngl` value. `-1` offloads all layers to GPU; `0` is CPU-only.
    pub n_gpu_layers: i32,
}

impl ModelSpec {
    /// Phase 3 default model.
    ///
    /// `n_gpu_layers = -1` offloads every layer to the GPU. The Windows install pulls the
    /// CUDA-enabled cu125 llama-cpp-python wheel from abetlen's GitHub releases (see
    /// `workers/py/pyproject.toml`) which requires CUDA Toolkit 12.5 system-wide; the
    /// `.nm-env.ps1` bootstrap prepends `CUDA\v12.5\bin` so cudart64_12.dll loads. On
    /// Linux/macOS the source build of the wheel may be CPU-only — callers can override
    /// via the `n_gpu_layers` arg on `start_inference_server` if CUDA isn't available.
    #[must_use]
    pub fn default_qwen2_5_7b_q4km() -> Self {
        Self {
            repo_id: "bartowski/Qwen2.5-7B-Instruct-GGUF".into(),
            filename: "Qwen2.5-7B-Instruct-Q4_K_M.gguf".into(),
            n_ctx: 4096,
            n_gpu_layers: -1,
        }
    }
}

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("model download failed: {0}")]
    Download(String),

    #[error("server spawn failed: {0}")]
    Spawn(String),

    #[error("server health check timed out after {0}s")]
    HealthTimeout(u64),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Status snapshot for UI / telemetry.
#[derive(Debug, Clone, Serialize)]
pub struct InferenceStatus {
    pub running: bool,
    pub repo_id: Option<String>,
    pub filename: Option<String>,
    pub port: Option<u16>,
    pub endpoint: Option<String>,
    pub started_unix_ms: Option<u128>,
    pub last_used_unix_ms: Option<u128>,
}

struct ServerState {
    child: Child,
    port: u16,
    model: ModelSpec,
    started_at: Instant,
    last_used: Arc<Mutex<Instant>>,
    model_path: PathBuf,
}

/// Singleton manager — one per `AppState`. Cheap to clone (`Arc`).
#[derive(Clone)]
pub struct InferenceManager {
    state: Arc<Mutex<Option<ServerState>>>,
}

impl Default for InferenceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl InferenceManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// Ensure a server is running for `model`. Returns the base URL
    /// (`http://127.0.0.1:<port>`). Downloads + spawns if not running, or if running with
    /// a different model id (kills the old one first).
    pub async fn ensure_running(
        &self,
        runner: &PythonRunner,
        model: &ModelSpec,
    ) -> Result<String, InferenceError> {
        let mut guard = self.state.lock().await;
        if let Some(s) = guard.as_ref() {
            if &s.model == model {
                *s.last_used.lock().await = Instant::now();
                return Ok(format!("http://127.0.0.1:{}", s.port));
            }
            // Different model — kill + restart.
            info!(old = %s.model.repo_id, new = %model.repo_id, "model change: stopping previous server");
            if let Some(old) = guard.take() {
                stop_state(old).await;
            }
        }

        // 1. Download via inference worker (idempotent — uses HF cache).
        let model_path = download_model(runner, model).await?;

        // 2. Pick a free port starting at DEFAULT_PORT.
        let port = pick_free_port(DEFAULT_PORT).await?;

        // 3. Spawn llama_cpp.server (long-lived; stdout/stderr inherited so logs flow to
        //    the orchestrator's tracing subscriber).
        let child = spawn_server(runner, &model_path, port, model)?;

        // 4. Wait for /health.
        wait_for_health(port, STARTUP_HEALTH_TIMEOUT).await?;

        let last_used = Arc::new(Mutex::new(Instant::now()));
        let new_state = ServerState {
            child,
            port,
            model: model.clone(),
            started_at: Instant::now(),
            last_used,
            model_path,
        };
        info!(port, repo = %model.repo_id, "inference server up");
        *guard = Some(new_state);
        Ok(format!("http://127.0.0.1:{port}"))
    }

    /// Bump the last-used timestamp. Call before every chat request so the TTL thread
    /// sees activity.
    pub async fn mark_used(&self) {
        let guard = self.state.lock().await;
        if let Some(s) = guard.as_ref() {
            *s.last_used.lock().await = Instant::now();
        }
    }

    /// Stop the server if running. Idempotent.
    pub async fn stop(&self) {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.take() {
            stop_state(state).await;
        }
    }

    /// Snapshot for status / UI.
    pub async fn status(&self) -> InferenceStatus {
        let guard = self.state.lock().await;
        match guard.as_ref() {
            None => InferenceStatus {
                running: false,
                repo_id: None,
                filename: None,
                port: None,
                endpoint: None,
                started_unix_ms: None,
                last_used_unix_ms: None,
            },
            Some(s) => {
                let last = *s.last_used.lock().await;
                InferenceStatus {
                    running: true,
                    repo_id: Some(s.model.repo_id.clone()),
                    filename: Some(s.model.filename.clone()),
                    port: Some(s.port),
                    endpoint: Some(format!("http://127.0.0.1:{}", s.port)),
                    started_unix_ms: Some(instant_to_unix_ms(s.started_at)),
                    last_used_unix_ms: Some(instant_to_unix_ms(last)),
                }
            }
        }
    }

    /// Helper for the TTL watchdog: returns true if a server is currently up *and* has
    /// been idle longer than `ttl`.
    pub async fn idle_past(&self, ttl: Duration) -> bool {
        let guard = self.state.lock().await;
        let Some(s) = guard.as_ref() else { return false };
        let last = *s.last_used.lock().await;
        last.elapsed() > ttl
    }

    /// One-shot idle check, intended to be called from a caller-owned loop. Returns
    /// `true` when the server existed *and* has been stopped on this tick.
    ///
    /// Spawning here is deliberately the caller's job: orchestrator stays decoupled
    /// from any specific async runtime (Tauri uses its own `async_runtime` wrapper;
    /// tests might use `tokio::test`). See `apps/desktop/src-tauri/src/lib.rs::run`
    /// for the production wiring.
    pub async fn check_idle_and_stop(&self, ttl: Duration) -> bool {
        if self.idle_past(ttl).await {
            info!(?ttl, "inference server idle past TTL; stopping");
            self.stop().await;
            return true;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn download_model(
    runner: &PythonRunner,
    model: &ModelSpec,
) -> Result<PathBuf, InferenceError> {
    let cmd = WorkerCommand {
        module: "narrowmind_workers.inference".into(),
        method: "inference.download_model".into(),
        params: serde_json::json!({
            "repo_id": model.repo_id,
            "filename": model.filename,
        }),
        // First-time download of a 7B Q4_K_M can take 15+ min on slow links.
        timeout: Some(Duration::from_secs(60 * 60)),
    };
    let value = call_worker(runner, &cmd)
        .await
        .map_err(|e| InferenceError::Download(e.to_string()))?;
    let path = value
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| InferenceError::Download("download response missing 'path'".into()))?;
    Ok(PathBuf::from(path))
}

fn spawn_server(
    runner: &PythonRunner,
    model_path: &std::path::Path,
    port: u16,
    model: &ModelSpec,
) -> Result<Child, InferenceError> {
    // We launch python ourselves (not via uv run python -m llama_cpp.server) by reusing
    // the runner's interpreter and just swapping the module + args.
    let mut cmd = Command::new(&runner.program);
    cmd.args(&runner.leading_args)
        .arg("-m")
        .arg("llama_cpp.server")
        .arg("--model")
        .arg(model_path)
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--n_ctx")
        .arg(model.n_ctx.to_string())
        .arg("--n_gpu_layers")
        .arg(model.n_gpu_layers.to_string())
        .current_dir(&runner.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for (k, v) in &runner.envs {
        cmd.env(k, v);
    }
    cmd.spawn().map_err(|e| InferenceError::Spawn(e.to_string()))
}

async fn wait_for_health(port: u16, deadline: Duration) -> Result<(), InferenceError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| InferenceError::Spawn(format!("reqwest client: {e}")))?;
    let url = format!("http://127.0.0.1:{port}/v1/models");
    let result = timeout(deadline, async {
        loop {
            if let Ok(r) = client.get(&url).send().await {
                if r.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(_) => Err(InferenceError::HealthTimeout(deadline.as_secs())),
    }
}

async fn pick_free_port(start: u16) -> Result<u16, InferenceError> {
    use tokio::net::TcpListener;
    for offset in 0..PORT_PROBE_LIMIT {
        let port = start.saturating_add(offset);
        if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
            return Ok(port);
        }
    }
    Err(InferenceError::Spawn(format!(
        "no free port found near {start} (probed {PORT_PROBE_LIMIT})"
    )))
}

async fn stop_state(mut state: ServerState) {
    // kill_on_drop is true so dropping the child also kills; the explicit start_kill +
    // wait lets us log the exit deterministically and drop the model file handle.
    if let Err(e) = state.child.start_kill() {
        warn!(error = %e, "inference server kill signal failed");
    }
    let _ = state.child.wait().await;
    info!(repo = %state.model.repo_id, "inference server stopped");
    drop(state.model_path);
}

fn instant_to_unix_ms(at: Instant) -> u128 {
    // Approximate wall-clock from Instant via a now-baseline. Within a single app run
    // this is monotonic and good enough for "time since started" displays.
    let now_inst = Instant::now();
    let now_wall = std::time::SystemTime::now();
    let delta = now_inst.saturating_duration_since(at);
    let wall = now_wall - delta;
    wall.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}
