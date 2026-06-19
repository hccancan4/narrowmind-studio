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
/// (Registry: `crate::retry::timeouts::INFERENCE_STARTUP_HEALTH`.)
const STARTUP_HEALTH_TIMEOUT: Duration = crate::retry::timeouts::INFERENCE_STARTUP_HEALTH;
/// Idle TTL — after this much time with no `mark_used()`, the server is killed.
pub const IDLE_TTL: Duration = Duration::from_secs(10 * 60);

/// KV-cache quantization for the llama.cpp server (Phase 4.6 efficiency Tier 1).
///
/// On the 8 GB reference card the K/V cache competes with the `Q4` weights for
/// VRAM; quantizing it frees room so longer RAG context (chunks, system prompt,
/// query) fits. `Q8_0` is near-lossless and roughly halves KV memory vs `F16` —
/// the default for the band. `F16` is the exact-baseline opt-out; `Q4_0` is the
/// aggressive opt-in (more headroom, measurable quality risk on a model already
/// at `Q4_K_M` weights).
///
/// The wire values are the ggml type ids llama-cpp-python's `--type_k` / `--type_v`
/// expect (GGML ABI: `F16`=1, `Q4_0`=2, `Q8_0`=8). We only emit the flags for the
/// quantized variants; `F16` leaves the server on its built-in default so the
/// opt-out is byte-for-byte the pre-4.6 serving behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum KvCacheType {
    /// Full-precision KV cache (llama.cpp default). Opt-out of quantization.
    F16,
    /// Near-lossless 8-bit KV cache. Default for the 8 GB band.
    #[default]
    Q8_0,
    /// Aggressive 4-bit KV cache — max context headroom, quality risk.
    Q4_0,
}

impl KvCacheType {
    /// ggml type id for `--type_k` / `--type_v`. `None` for `F16`: we leave the
    /// flag off so the server keeps its own f16 default rather than pinning it
    /// (keeps the opt-out identical to pre-4.6 behavior).
    #[must_use]
    pub fn ggml_type_id(self) -> Option<i32> {
        match self {
            Self::F16 => None,
            Self::Q4_0 => Some(2),
            Self::Q8_0 => Some(8),
        }
    }

    /// llama.cpp requires Flash Attention for a quantized **value** cache, so any
    /// non-f16 cache implies `--flash_attn true`. (Key-only quant would not, but
    /// we always quantize K and V together — see [`build_server_args`].)
    #[must_use]
    pub fn requires_flash_attn(self) -> bool {
        self.ggml_type_id().is_some()
    }

    /// Parse a tool-arg wire string. `None` for unknown input — the caller
    /// surfaces the accepted set rather than guessing.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "f16" => Some(Self::F16),
            "q8_0" | "q8" => Some(Self::Q8_0),
            "q4_0" | "q4" => Some(Self::Q4_0),
            _ => None,
        }
    }

    /// Canonical wire string (matches the serde representation). For logs / UI.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::F16 => "f16",
            Self::Q8_0 => "q8_0",
            Self::Q4_0 => "q4_0",
        }
    }
}

/// Where to pull the GGUF from. Defaults from Phase 3 decision A (Qwen2.5-7B `Q4_K_M`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSpec {
    pub repo_id: String,
    pub filename: String,
    /// Context window for the server. 4096 is a safe default for 7B `Q4_K_M` on 8 GB.
    pub n_ctx: u32,
    /// llama.cpp `-ngl` value. `-1` offloads all layers to GPU; `0` is CPU-only.
    pub n_gpu_layers: i32,
    /// KV-cache quantization (Phase 4.6). `Q8_0` by default on the reference card.
    /// Changing it (vs a running server) is a model-identity change → restart.
    #[serde(default)]
    pub kv_cache_type: KvCacheType,
    /// Force Flash Attention even with an f16 cache (a speed/VRAM win on Ampere).
    /// `false` by default; a quantized `kv_cache_type` turns it on regardless via
    /// [`KvCacheType::requires_flash_attn`].
    #[serde(default)]
    pub flash_attn: bool,
    /// Reuse the llama.cpp prompt KV cache across requests (Phase 4.6) so the
    /// shared system prompt + repeated RAG context aren't re-evaluated on every
    /// call. This is the server's host-RAM cache (`cache_type=ram`) — no VRAM
    /// cost — and is on by default.
    #[serde(default = "default_prompt_cache")]
    pub prompt_cache: bool,
}

/// serde default for [`ModelSpec::prompt_cache`] — on. A bare `#[serde(default)]`
/// would give `false`; the 4.6 default is to reuse the prompt cache.
fn default_prompt_cache() -> bool {
    true
}

impl ModelSpec {
    /// Phase 3 default model — now a thin delegation into the base model
    /// registry (`crate::models`), kept under its historical name so the two
    /// call sites (start_inference_server tool, chat_preview_bootstrap) don't
    /// churn. A registry unit test pins the output field-for-field to the
    /// pre-registry literals, so this indirection is provably behavior-neutral.
    ///
    /// `n_gpu_layers = -1` offloads every layer to the GPU. The Windows install pulls the
    /// CUDA-enabled cu125 llama-cpp-python wheel from abetlen's GitHub releases (see
    /// `workers/py/pyproject.toml`) which requires CUDA Toolkit 12.5 system-wide; the
    /// `.nm-env.ps1` bootstrap prepends `CUDA\v12.5\bin` so cudart64_12.dll loads. On
    /// Linux/macOS the source build of the wheel may be CPU-only — callers can override
    /// via the `n_gpu_layers` arg on `start_inference_server` if CUDA isn't available.
    #[must_use]
    pub fn default_qwen2_5_7b_q4km() -> Self {
        crate::models::default_model().to_model_spec()
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

/// A local domain GGUF (exported by `export_domain_gguf`) is served directly:
/// if `filename` is an absolute path to an existing file, it isn't an HF repo
/// filename. Returns the path to serve, or `None` to fall through to a download.
/// (Phase 4.6 slice 3c.)
fn local_model_path(filename: &str) -> Option<PathBuf> {
    let p = PathBuf::from(filename);
    (p.is_absolute() && p.is_file()).then_some(p)
}

async fn download_model(
    runner: &PythonRunner,
    model: &ModelSpec,
) -> Result<PathBuf, InferenceError> {
    // Local domain GGUF — skip the HuggingFace round-trip entirely.
    if let Some(local) = local_model_path(&model.filename) {
        info!(path = %local.display(), "serving local GGUF (no download)");
        return Ok(local);
    }
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

/// Build the `llama_cpp.server` argv that follows `-m llama_cpp.server`.
///
/// Extracted from [`spawn_server`] so the flag wiring is unit-testable without
/// spawning a process — in particular the KV-cache-quant + Flash-Attention
/// coupling (Phase 4.6). Note: `llama_cpp.server`'s pydantic CLI parses bool
/// fields with a value (`--flash_attn true`), NOT as store-true, and `type_k`/
/// `type_v` take ggml type ids as integers.
fn build_server_args(model_path: &std::path::Path, port: u16, model: &ModelSpec) -> Vec<String> {
    let mut args = vec![
        "--model".into(),
        model_path.to_string_lossy().into_owned(),
        "--port".into(),
        port.to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--n_ctx".into(),
        model.n_ctx.to_string(),
        "--n_gpu_layers".into(),
        model.n_gpu_layers.to_string(),
    ];
    // KV-cache quantization: K and V together (a quantized V needs Flash Attn).
    if let Some(type_id) = model.kv_cache_type.ggml_type_id() {
        args.push("--type_k".into());
        args.push(type_id.to_string());
        args.push("--type_v".into());
        args.push(type_id.to_string());
    }
    // Flash Attention: required by a quantized V cache; also honoured as an
    // explicit f16 speed opt-in. Only emitted when true (server default is false).
    if model.flash_attn || model.kv_cache_type.requires_flash_attn() {
        args.push("--flash_attn".into());
        args.push("true".into());
    }
    // Prompt-prefix cache reuse (host RAM, not VRAM). Server default is off, so
    // we emit it explicitly; cache_type defaults to "ram" which is what we want.
    if model.prompt_cache {
        args.push("--cache".into());
        args.push("true".into());
    }
    args
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
        .args(build_server_args(model_path, port, model))
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
        .timeout(crate::retry::timeouts::INFERENCE_HEALTH_PROBE)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kv: KvCacheType, flash_attn: bool) -> ModelSpec {
        ModelSpec {
            repo_id: "r".into(),
            filename: "m.gguf".into(),
            n_ctx: 4096,
            n_gpu_layers: -1,
            kv_cache_type: kv,
            flash_attn,
            prompt_cache: true,
        }
    }

    /// Helper: index of `flag` in argv, asserting its value arg follows.
    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str())
    }

    #[test]
    fn ggml_type_ids_match_abi() {
        // These are GGML ABI constants the llama.cpp server consumes verbatim.
        assert_eq!(KvCacheType::F16.ggml_type_id(), None);
        assert_eq!(KvCacheType::Q8_0.ggml_type_id(), Some(8));
        assert_eq!(KvCacheType::Q4_0.ggml_type_id(), Some(2));
    }

    #[test]
    fn quantized_kv_requires_flash_attn_but_f16_does_not() {
        assert!(!KvCacheType::F16.requires_flash_attn());
        assert!(KvCacheType::Q8_0.requires_flash_attn());
        assert!(KvCacheType::Q4_0.requires_flash_attn());
    }

    #[test]
    fn q8_0_emits_type_flags_and_flash_attn() {
        let args = build_server_args(std::path::Path::new("/tmp/m.gguf"), 8765, &spec(KvCacheType::Q8_0, false));
        assert_eq!(value_after(&args, "--type_k"), Some("8"));
        assert_eq!(value_after(&args, "--type_v"), Some("8"));
        // Bool flags take a value in llama_cpp.server's CLI — never store-true.
        assert_eq!(value_after(&args, "--flash_attn"), Some("true"));
    }

    #[test]
    fn q4_0_emits_type_2() {
        let args = build_server_args(std::path::Path::new("/tmp/m.gguf"), 8765, &spec(KvCacheType::Q4_0, false));
        assert_eq!(value_after(&args, "--type_k"), Some("2"));
        assert_eq!(value_after(&args, "--type_v"), Some("2"));
        assert_eq!(value_after(&args, "--flash_attn"), Some("true"));
    }

    #[test]
    fn f16_default_omits_kv_and_flash_flags() {
        // The exact-baseline opt-out: no --type_k/--type_v/--flash_attn at all,
        // so serving is byte-for-byte the pre-4.6 behavior.
        let args = build_server_args(std::path::Path::new("/tmp/m.gguf"), 8765, &spec(KvCacheType::F16, false));
        assert!(!args.iter().any(|a| a == "--type_k"));
        assert!(!args.iter().any(|a| a == "--type_v"));
        assert!(!args.iter().any(|a| a == "--flash_attn"));
    }

    #[test]
    fn f16_with_explicit_flash_attn_emits_only_flash() {
        // Speed opt-in without KV quant: flash on, no type flags.
        let args = build_server_args(std::path::Path::new("/tmp/m.gguf"), 8765, &spec(KvCacheType::F16, true));
        assert!(!args.iter().any(|a| a == "--type_k"));
        assert_eq!(value_after(&args, "--flash_attn"), Some("true"));
    }

    #[test]
    fn base_args_always_present() {
        let args = build_server_args(std::path::Path::new("/models/x.gguf"), 9001, &spec(KvCacheType::Q8_0, false));
        assert_eq!(value_after(&args, "--model"), Some("/models/x.gguf"));
        assert_eq!(value_after(&args, "--port"), Some("9001"));
        assert_eq!(value_after(&args, "--host"), Some("127.0.0.1"));
        assert_eq!(value_after(&args, "--n_ctx"), Some("4096"));
        assert_eq!(value_after(&args, "--n_gpu_layers"), Some("-1"));
    }

    #[test]
    fn prompt_cache_on_emits_cache_flag() {
        // Default helper has prompt_cache = true. Bool flag takes a value.
        let args = build_server_args(std::path::Path::new("/tmp/m.gguf"), 8765, &spec(KvCacheType::Q8_0, false));
        assert_eq!(value_after(&args, "--cache"), Some("true"));
    }

    #[test]
    fn prompt_cache_off_omits_cache_flag() {
        let mut s = spec(KvCacheType::Q8_0, false);
        s.prompt_cache = false;
        let args = build_server_args(std::path::Path::new("/tmp/m.gguf"), 8765, &s);
        assert!(!args.iter().any(|a| a == "--cache"));
    }

    #[test]
    fn kv_cache_type_serde_wire_strings() {
        // Wire strings are the project.toml / JSON contract — pin them.
        assert_eq!(serde_json::to_string(&KvCacheType::F16).unwrap(), "\"f16\"");
        assert_eq!(serde_json::to_string(&KvCacheType::Q8_0).unwrap(), "\"q8_0\"");
        assert_eq!(serde_json::to_string(&KvCacheType::Q4_0).unwrap(), "\"q4_0\"");
        assert_eq!(KvCacheType::default(), KvCacheType::Q8_0);
    }

    #[test]
    fn from_wire_accepts_aliases_and_rejects_unknown() {
        assert_eq!(KvCacheType::from_wire("Q8_0"), Some(KvCacheType::Q8_0));
        assert_eq!(KvCacheType::from_wire("q8"), Some(KvCacheType::Q8_0));
        assert_eq!(KvCacheType::from_wire(" f16 "), Some(KvCacheType::F16));
        assert_eq!(KvCacheType::from_wire("q4"), Some(KvCacheType::Q4_0));
        assert_eq!(KvCacheType::from_wire("q2_k"), None);
        assert_eq!(KvCacheType::from_wire(""), None);
    }

    #[test]
    fn modelspec_missing_efficiency_fields_use_46_defaults() {
        // Back-compat: a serialized spec from before 4.6 has no kv/cache fields.
        // They must fill in the 4.6 defaults (q8_0 KV, prompt cache on).
        let json = r#"{"repo_id":"r","filename":"m.gguf","n_ctx":4096,"n_gpu_layers":-1}"#;
        let spec: ModelSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.kv_cache_type, KvCacheType::Q8_0);
        assert!(!spec.flash_attn);
        assert!(spec.prompt_cache);
    }

    #[test]
    fn local_model_path_detects_existing_absolute_gguf() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("domain-q4_k_m.gguf");
        std::fs::write(&f, b"gguf").unwrap();
        // absolute path to an existing file -> serve it directly
        assert_eq!(local_model_path(f.to_str().unwrap()), Some(f.clone()));
        // absolute but missing -> None (fall through to HF download)
        let missing = tmp.path().join("missing.gguf");
        assert_eq!(local_model_path(&missing.to_string_lossy()), None);
        // a plain HF repo filename (relative) -> None
        assert_eq!(local_model_path("Qwen2.5-7B-Instruct-Q4_K_M.gguf"), None);
    }
}
