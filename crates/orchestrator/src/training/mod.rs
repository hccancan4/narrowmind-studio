//! Training lifecycle: the `TrainingManager` singleton, hardware presets, and
//! WSL interop for the Phase 4 QLoRA worker.
//!
//! Mirrors `InferenceManager`'s shape (one `Arc`-shared manager on `AppState`,
//! `Mutex<Option<state>>` inside) with training-specific semantics:
//!
//! - **VRAM hard mutex (KARAR 1)**: 8 GB cannot hold the llama.cpp server
//!   (~5 GB) and a QLoRA run (~6-7 GB) together. `start()` stops a running
//!   inference server first and writes the fact to agent.log. Training NEVER
//!   auto-restarts inference afterwards — the completion message points the
//!   user at `start_inference_server` / the Local chat button instead.
//! - **One run at a time**: a second `start()` while active is an error.
//! - **Filesystem is the source of truth**: the Python worker owns
//!   `runs/<id>/status.json` + `metrics.jsonl`; the manager mirrors a small
//!   in-memory snapshot for cheap status queries and emits live events for
//!   the UI, but after an app restart the run history is read from disk.

pub mod preset;
pub mod wsl;

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex};
use tracing::{info, warn};

use crate::error::WorkerError;
use crate::inference::InferenceManager;
use crate::models::BaseModel;
use crate::project::TrainingConfig;
use crate::training::preset::TrainingPreset;
use crate::worker::{call_worker_streaming, PythonRunner, WorkerCommand};

/// Assemble the worker's `config` param: preset values, overridden by the
/// project's `[training]` section, joined with the registry model's identity
/// fields. Field names mirror the Python `TrainingParams` dataclass exactly —
/// a rename on either side is a contract break (pinned by tests both sides).
#[must_use]
pub fn worker_config(
    cfg: &TrainingConfig,
    preset: &TrainingPreset,
    base: &BaseModel,
    seed: u64,
) -> Value {
    json!({
        "base_model_hf_repo": base.hf_repo,
        "tokenizer_id": base.tokenizer_id,
        "chat_template": base.chat_template.as_str(),
        "load_in_4bit": preset.load_in_4bit,
        "lora_r": cfg.lora_r.unwrap_or(preset.lora_r),
        "lora_alpha": cfg.lora_alpha.unwrap_or(preset.lora_alpha),
        "target_modules": preset.target_modules,
        "per_device_train_batch_size": cfg.per_device_train_batch_size.unwrap_or(preset.per_device_train_batch_size),
        "gradient_accumulation_steps": cfg.gradient_accumulation_steps.unwrap_or(preset.gradient_accumulation_steps),
        "max_seq_length": cfg.max_seq_length.unwrap_or(preset.max_seq_length),
        "learning_rate": cfg.learning_rate.unwrap_or(preset.learning_rate),
        "warmup_ratio": preset.warmup_ratio,
        "num_train_epochs": cfg.num_train_epochs.unwrap_or(preset.num_train_epochs),
        "lr_scheduler_type": preset.lr_scheduler_type,
        "bf16": preset.bf16,
        "optim": preset.optim,
        "gradient_checkpointing": preset.gradient_checkpointing,
        "seed": seed,
        "validation_split": preset.validation_split,
        "save_total_limit": preset.save_total_limit,
    })
}

/// Channel capacity for live training events. Slow UI subscribers lose old
/// events (broadcast lag) — acceptable: metrics.jsonl is the durable record.
const EVENT_CAPACITY: usize = 256;

/// Live event forwarded to the desktop layer (→ Tauri "training:event").
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrainingEvent {
    /// Worker stage transitions (dataset, loading_model, tokenized, training).
    Stage { run_id: String, data: Value },
    /// Per-step metric record — same shape as a metrics.jsonl line.
    Metric { run_id: String, data: Value },
    /// Adapter test progress (M6).
    AdapterAnswer { run_id: String, data: Value },
    /// Terminal state reached.
    Finished {
        run_id: String,
        status: String,
        /// Human-oriented summary for the transcript / UI toast.
        message: String,
    },
}

/// Cheap status snapshot for tools / Tauri commands.
#[derive(Debug, Clone, Serialize)]
pub struct TrainingStatus {
    pub active: bool,
    pub run_id: Option<String>,
    pub project: Option<String>,
    pub step: u64,
    pub total_steps: u64,
    pub epoch: f64,
}

struct ActiveRun {
    run_id: String,
    project_name: String,
    cancel_tx: watch::Sender<bool>,
    /// Mirrored from training.metric notifications.
    step: u64,
    total_steps: u64,
    epoch: f64,
}

/// Singleton manager — one per `AppState`. Cheap to clone via `Arc`.
pub struct TrainingManager {
    state: Mutex<Option<ActiveRun>>,
    events: broadcast::Sender<TrainingEvent>,
}

impl Default for TrainingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TrainingManager {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            state: Mutex::new(None),
            events,
        }
    }

    /// Subscribe to live events (desktop forwards these to the UI).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<TrainingEvent> {
        self.events.subscribe()
    }

    pub async fn is_active(&self) -> bool {
        self.state.lock().await.is_some()
    }

    pub async fn status(&self) -> TrainingStatus {
        match self.state.lock().await.as_ref() {
            Some(run) => TrainingStatus {
                active: true,
                run_id: Some(run.run_id.clone()),
                project: Some(run.project_name.clone()),
                step: run.step,
                total_steps: run.total_steps,
                epoch: run.epoch,
            },
            None => TrainingStatus {
                active: false,
                run_id: None,
                project: None,
                step: 0,
                total_steps: 0,
                epoch: 0.0,
            },
        }
    }

    /// Launch a QLoRA run. Returns the new `run_id` immediately — the run
    /// itself executes on a background task; progress flows via
    /// [`TrainingEvent`]s and the run directory.
    ///
    /// `base_model` comes from the registry (caller already validated
    /// `training_supported`); `cfg` is the project's `[training]` section
    /// (preset + overrides already merged by the caller via
    /// [`TrainingConfig::resolved`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        self: &Arc<Self>,
        runner: Arc<PythonRunner>,
        project_name: String,
        project_root: PathBuf,
        worker_cfg: Value,
        idle_timeout: std::time::Duration,
        resume_from: Option<String>,
        inference: Arc<InferenceManager>,
    ) -> Result<String, String> {
        let mut state = self.state.lock().await;
        if let Some(active) = state.as_ref() {
            return Err(format!(
                "a training run is already active (run {}, step {}/{}); stop it first",
                active.run_id, active.step, active.total_steps
            ));
        }

        // --- VRAM hard mutex (KARAR 1) -------------------------------------
        if inference.status().await.running {
            inference.stop().await;
            let msg = "inference server stopped to free VRAM for training";
            crate::tools::util::log_to_agent_log(&project_root, msg);
            info!("{msg}");
        }

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let run_dir = project_root.join("runs").join(&run_id);
        std::fs::create_dir_all(&run_dir).map_err(|e| format!("create run dir: {e}"))?;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        *state = Some(ActiveRun {
            run_id: run_id.clone(),
            project_name: project_name.clone(),
            cancel_tx,
            step: 0,
            total_steps: 0,
            epoch: 0.0,
        });
        drop(state);

        // --- Worker params ---------------------------------------------------
        // project_root crosses into WSL: translate to /mnt/<drive>/ form.
        let params = json!({
            "project_root": wsl::to_wsl_path(&project_root),
            "run_id": run_id,
            "resume_from": resume_from,
            "config": worker_cfg,
        });
        let cmd = WorkerCommand {
            module: "narrowmind_workers.training".into(),
            method: "training.run".into(),
            params,
            timeout: None, // streaming path uses idle_timeout below
        };
        let idle = idle_timeout;

        // --- Background driver -----------------------------------------------
        let manager = Arc::clone(self);
        let events = self.events.clone();
        let run_id_task = run_id.clone();
        tokio::spawn(async move {
            let events_cb = events.clone();
            let manager_cb = Arc::clone(&manager);
            let run_id_cb = run_id_task.clone();

            // Bridge: sync callback → async state update via std try_lock-free
            // channel. Simplest correct option: an unbounded mpsc consumed by
            // a sibling task that owns the async mutex updates.
            let (notify_tx, mut notify_rx) =
                tokio::sync::mpsc::unbounded_channel::<(String, Value)>();
            let mirror = tokio::spawn(async move {
                while let Some((method, params)) = notify_rx.recv().await {
                    match method.as_str() {
                        "training.metric" => {
                            if let Some(run) = manager_cb.state.lock().await.as_mut() {
                                run.step = params.get("step").and_then(Value::as_u64).unwrap_or(run.step);
                                run.total_steps = params
                                    .get("total_steps")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(run.total_steps);
                                run.epoch =
                                    params.get("epoch").and_then(Value::as_f64).unwrap_or(run.epoch);
                            }
                            let _ = events_cb.send(TrainingEvent::Metric {
                                run_id: run_id_cb.clone(),
                                data: params,
                            });
                        }
                        "training.stage" => {
                            let _ = events_cb.send(TrainingEvent::Stage {
                                run_id: run_id_cb.clone(),
                                data: params,
                            });
                        }
                        "training.adapter_answer" => {
                            let _ = events_cb.send(TrainingEvent::AdapterAnswer {
                                run_id: run_id_cb.clone(),
                                data: params,
                            });
                        }
                        other => {
                            warn!(method = other, "unknown training notification");
                        }
                    }
                }
            });

            let result = call_worker_streaming(
                &runner,
                &cmd,
                idle,
                cancel_rx,
                move |method, params| {
                    let _ = notify_tx.send((method.to_string(), params.clone()));
                },
            )
            .await;
            drop(mirror); // channel sender dropped above when callback drops; let task finish
            // (mirror task ends when the channel closes; aborting would race final events)

            let (status, message) = match &result {
                Ok(value) => {
                    let best = value
                        .get("best_eval_loss")
                        .and_then(Value::as_f64)
                        .map_or(String::new(), |b| format!(" (best eval_loss {b:.4})"));
                    (
                        "completed".to_string(),
                        format!(
                            "training finished{best}; restart inference with start_inference_server or the Local chat button"
                        ),
                    )
                }
                Err(WorkerError::Cancelled { .. }) => (
                    "cancelled".to_string(),
                    "training cancelled; partial checkpoints remain in the run directory".to_string(),
                ),
                Err(e) => ("failed".to_string(), format!("training failed: {e}")),
            };

            // The worker writes its own terminal status.json on success/handler
            // failure; cancel/kill paths can't, so patch it from here.
            if status != "completed" {
                patch_status_file(&project_root, &run_id_task, &status, &message);
            }
            crate::tools::util::log_to_agent_log(
                &project_root,
                &format!("training run {run_id_task}: {status} — {message}"),
            );
            // Persist last_run_id for the project (best-effort).
            let _ = persist_last_run_id(&project_root, &run_id_task);

            *manager.state.lock().await = None;
            let _ = events.send(TrainingEvent::Finished {
                run_id: run_id_task,
                status,
                message,
            });
        });

        Ok(run_id)
    }

    /// Request cancellation of the active run. Returns false when idle.
    pub async fn stop(&self) -> bool {
        match self.state.lock().await.as_ref() {
            Some(run) => {
                info!(run_id = %run.run_id, "training stop requested");
                let _ = run.cancel_tx.send(true);
                true
            }
            None => false,
        }
    }
}

/// Merge a terminal status into runs/<id>/status.json without clobbering the
/// worker-written fields (mirrors the Python write_status merge semantics).
fn patch_status_file(project_root: &std::path::Path, run_id: &str, status: &str, message: &str) {
    let path = project_root.join("runs").join(run_id).join("status.json");
    let mut current: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    current.insert("run_id".into(), json!(run_id));
    current.insert("status".into(), json!(status));
    current.insert("message".into(), json!(message));
    if let Ok(s) = serde_json::to_string_pretty(&Value::Object(current)) {
        let _ = std::fs::write(&path, s);
    }
}

/// Record `[training] last_run_id` in project.toml (KARAR 5).
fn persist_last_run_id(project_root: &std::path::Path, run_id: &str) -> Result<(), String> {
    // project.toml lives at the project root; reuse ProjectStore semantics
    // without holding a store handle: read-modify-write through the config
    // types so schema validation still applies.
    let path = project_root.join("project.toml");
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut cfg: crate::project::ProjectConfig =
        toml::from_str(&raw).map_err(|e| e.to_string())?;
    cfg.training.last_run_id = Some(run_id.to_string());
    let out = toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, out).map_err(|e| e.to_string())
}
