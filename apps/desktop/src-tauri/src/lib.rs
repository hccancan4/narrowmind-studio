//! Tauri v2 shell. Hosts the Phase 1 agent loop, project store, secrets, and tools.
//!
//! The desktop crate is intentionally thin: every command in [`commands`] either reads
//! `AppState` and routes to the orchestrator, or wires an event channel from the agent /
//! tool layer to a Tauri event the front-end subscribes to.

use std::path::PathBuf;
use std::sync::Arc;

use narrowmind_orchestrator::ProjectStore;

mod commands;
mod state;
mod system_prompt {} // anchored module just so the include_str! path resolves cleanly

use crate::state::AppState;

fn workspace_root() -> Option<PathBuf> {
    // src-tauri lives at apps/desktop/src-tauri; the workspace root is three levels up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.ancestors().nth(3).map(Path::to_path_buf)
}

use std::path::Path;

/// Build and run the Tauri application. Returns only on shutdown.
///
/// # Panics
/// Panics if Tauri's runtime cannot be constructed (missing webview, OS-level failure) or if
/// the OS data directory cannot be located. At that point there is no usable UI to surface
/// the error through, so terminating is the only option.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    // Dev affordance: NARROWMIND_PROJECTS_ROOT overrides the OS-default
    // projects directory. Needed whenever project data must be visible to
    // BOTH Windows and WSL2 (Phase 4 training workers read the project dir
    // through /mnt/c) from a working tree that isn't under the user profile —
    // e.g. sandboxed dev sessions, or users who keep projects on another
    // drive. Documented in docs/dev-setup.md.
    let store_root = std::env::var_os("NARROWMIND_PROJECTS_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            ProjectStore::default_root()
                .expect("could not determine default project root for this OS")
        });
    tracing::info!(root = %store_root.display(), "projects root");
    let project_store = Arc::new(ProjectStore::new(store_root));
    let ws_root =
        workspace_root().expect("could not locate workspace root from desktop crate manifest path");
    let app_state = AppState::new(project_store, ws_root);

    tauri::Builder::default()
        .manage(app_state)
        .setup(|app| {
            // Spawn the inference idle watchdog using Tauri's async runtime (not
            // tokio::spawn directly — the latter panics in .setup() because no tokio
            // current-runtime is set there). Tauri's wrapper enqueues onto its managed
            // tokio rt, where tokio::time::sleep / mpsc work as expected.
            use tauri::Emitter as _;
            use tauri::Manager as _;
            let state: tauri::State<'_, AppState> = app.state();
            let inference = state.inference.as_ref().clone();
            let ttl = narrowmind_orchestrator::INFERENCE_IDLE_TTL;
            tauri::async_runtime::spawn(async move {
                let tick = ttl.checked_div(6).unwrap_or(std::time::Duration::from_secs(60));
                loop {
                    tokio::time::sleep(tick).await;
                    inference.check_idle_and_stop(ttl).await;
                }
            });

            // Forward live training events to the UI ("training:event"). The
            // broadcast subscription outlives any single agent turn, so the
            // Training Monitor keeps streaming after the tool call returns.
            let mut training_events = state.training.subscribe();
            let app_for_training = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match training_events.recv().await {
                        Ok(ev) => {
                            let _ = app_for_training.emit("training:event", &ev);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "training event subscriber lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Orphan scan (M5 / KARAR 6): a previous app session may have died
            // mid-training. Dead pids are patched to failed in place; live
            // WSL-side processes are surfaced to the user for a confirmed kill
            // — never killed silently, never re-attached (v1).
            let projects_root = state.project_store.root().to_path_buf();
            let app_for_orphans = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let orphans =
                    narrowmind_orchestrator::training::scan_orphans(&projects_root).await;
                for orphan in orphans {
                    let _ = app_for_orphans.emit("training:orphan", &orphan);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // settings
            commands::settings::set_provider_key,
            commands::settings::has_provider_key,
            commands::settings::delete_provider_key,
            // projects
            commands::projects::list_projects,
            commands::projects::create_project,
            commands::projects::delete_project,
            commands::projects::select_project,
            commands::projects::current_project,
            commands::projects::project_status,
            commands::projects::get_project_provider,
            commands::projects::set_project_synth_model,
            // agent
            commands::agent::agent_send_message,
            commands::agent::agent_reset,
            commands::agent::agent_turn_count,
            // chunks (dataset browser)
            commands::chunks::list_chunks_cmd,
            commands::chunks::filter_chunks_cmd,
            // chat preview
            commands::chat::chat_preview_send,
            commands::chat::chat_preview_context,
            commands::chat::chat_preview_bootstrap,
            commands::chat::chat_preview_models,
            // training monitor
            commands::training::training_status,
            commands::training::training_runs,
            commands::training::training_metrics,
            commands::training::training_log_tail,
            commands::training::training_stop,
            commands::training::training_kill_orphan,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Deterministic cleanup on app exit: reap pooled Python workers and
            // the llama.cpp server so nothing outlives the window. kill_on_drop
            // is the backstop; this is the explicit, graceful path (stdin EOF →
            // clean Python exit → no orphan python.exe in Task Manager).
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager as _;
                let state: tauri::State<'_, AppState> = app_handle.state();
                let pool = state.worker_pool.clone();
                let inference = state.inference.clone();
                let training = state.training.clone();
                tauri::async_runtime::block_on(async move {
                    // Request training cancellation first: the streaming call's
                    // cancel path kills the WSL child and patches status.json,
                    // so the run reads "cancelled" (not orphaned) on next start.
                    training.stop().await;
                    pool.shutdown().await;
                    inference.stop().await;
                });
            }
        });
}
