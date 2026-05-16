//! Tauri v2 shell. Hosts the Phase 0 debug command that round-trips a `hello` call through the
//! Rust orchestrator into a Python worker subprocess.

use std::path::PathBuf;

use narrowmind_orchestrator::{hello_round_trip, HelloResult, PythonRunner};
use serde::Serialize;
use tracing::{error, info};

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

/// Invoked from the React UI's "Run hello round-trip" button.
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

/// Reports the orchestrator crate version so the UI can show build info.
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
/// Panics if Tauri's runtime cannot be constructed (missing webview, OS-level failure). At that
/// point there is no usable UI to surface the error through, so terminating is the only option.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            hello_round_trip_cmd,
            orchestrator_version
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
