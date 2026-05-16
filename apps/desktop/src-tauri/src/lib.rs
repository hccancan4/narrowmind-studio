//! Tauri v2 shell. Hosts the Phase 1 agent loop, project store, secrets, and tools.
//!
//! The desktop crate is intentionally thin: every command in [`commands`] either reads
//! `AppState` and routes to the orchestrator, or wires an event channel from the agent /
//! tool layer to a Tauri event the front-end subscribes to.

use std::path::PathBuf;
use std::sync::Arc;

use narrowmind_orchestrator::{hello_round_trip, HelloResult, ProjectStore, PythonRunner};
use serde::Serialize;
use tracing::{error, info};

mod commands;
mod state;
mod system_prompt {} // anchored module just so the include_str! path resolves cleanly

use crate::state::AppState;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct HelloPayload {
    pub message: String,
    pub worker_version: String,
    pub worker_pid: i64,
    pub python_version: String,
    pub platform: String,
}

impl From<HelloResult> for HelloPayload {
    fn from(r: HelloResult) -> Self {
        Self {
            message: r.message,
            worker_version: r.worker_version,
            worker_pid: r.worker_pid,
            python_version: r.python_version,
            platform: r.platform,
        }
    }
}

#[tauri::command]
async fn hello_round_trip_cmd(name: Option<String>) -> Result<HelloPayload, String> {
    let root = workspace_root().ok_or_else(|| {
        "could not locate workspace root from desktop crate manifest path".to_string()
    })?;
    info!(workspace = %root.display(), name = name.as_deref().unwrap_or("world"), "hello round-trip");
    let runner = PythonRunner::uv_workspace(root);
    match hello_round_trip(&runner, name.as_deref()).await {
        Ok(result) => Ok(result.into()),
        Err(e) => {
            error!(error = %e, "hello round-trip failed");
            Err(e.to_string())
        }
    }
}

#[tauri::command]
fn orchestrator_version() -> &'static str {
    narrowmind_orchestrator::version()
}

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

    let store_root = ProjectStore::default_root()
        .expect("could not determine default project root for this OS");
    let project_store = Arc::new(ProjectStore::new(store_root));
    let ws_root =
        workspace_root().expect("could not locate workspace root from desktop crate manifest path");
    let app_state = AppState::new(project_store, ws_root);

    tauri::Builder::default()
        .manage(app_state)
        .setup(|app| {
            // Tauri 2 brings up its tokio runtime before .setup() runs, so this is the
            // earliest safe point to spawn long-lived async tasks. We do it here rather
            // than inside AppState::new() (which runs in sync context and would panic
            // with 'there is no reactor running').
            use tauri::Manager as _;
            let state: tauri::State<'_, AppState> = app.state();
            let _watchdog = state
                .inference
                .as_ref()
                .clone()
                .spawn_ttl_watcher(narrowmind_orchestrator::INFERENCE_IDLE_TTL);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hello_round_trip_cmd,
            orchestrator_version,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
