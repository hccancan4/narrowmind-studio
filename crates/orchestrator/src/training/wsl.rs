//! WSL2 interop for the training worker.
//!
//! Unsloth's wheels mark native Windows unsupported, so the Phase 4 training
//! environment lives in WSL2 Ubuntu (see docs/dev-setup.md § Training
//! environment). The orchestrator talks to it through `wsl.exe -e bash
//! ./run-worker.sh ...` — same JSON-RPC-over-stdio protocol, the pipes pass
//! through wsl.exe transparently.

use std::path::Path;

use crate::worker::PythonRunner;

/// Translate a Windows path to its WSL `/mnt/<drive>/...` equivalent.
///
/// `C:\Users\PC\projects\x` → `/mnt/c/Users/PC/projects/x`. Paths that are
/// already POSIX-looking pass through unchanged (useful in tests on
/// non-Windows hosts). UNC paths are rejected by returning the input —
/// callers upstream (run_command hardening, tool validation) already refuse
/// UNC, so this is belt-and-suspenders.
#[must_use]
pub fn to_wsl_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        format!("/mnt/{}{}", drive, &s[2..])
    } else {
        s
    }
}

/// Build the `PythonRunner` that spawns the training worker inside WSL.
///
/// Shape produced by `build_command`:
///   `wsl.exe -e bash ./run-worker.sh -m narrowmind_workers.training`
/// with the Windows cwd at `workers/py-training` (wsl.exe auto-translates the
/// working directory). run-worker.sh owns the env decisions (venv on ext4,
/// HF cache on ext4, PYTHONPATH to the source tree) — see that script's
/// header for the rationale of each.
#[must_use]
pub fn training_runner(workspace_root: &Path) -> PythonRunner {
    PythonRunner {
        program: "wsl.exe".into(),
        leading_args: vec!["-e".into(), "bash".into(), "./run-worker.sh".into()],
        cwd: workspace_root.join("workers").join("py-training"),
        envs: std::collections::BTreeMap::new(), // env lives in run-worker.sh
    }
}

/// Is a Linux pid (inside the default WSL distro) still alive?
/// `kill -0` touches nothing; exit status carries the answer.
pub async fn wsl_pid_alive(pid: u32) -> bool {
    tokio::process::Command::new("wsl.exe")
        .args(["-e", "kill", "-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Kill a WSL-side pid (SIGKILL). Best-effort.
pub async fn wsl_kill(pid: u32) -> bool {
    tokio::process::Command::new("wsl.exe")
        .args(["-e", "kill", "-9", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn windows_drive_paths_translate() {
        assert_eq!(
            to_wsl_path(&PathBuf::from(r"C:\Users\PC\projects\demo")),
            "/mnt/c/Users/PC/projects/demo"
        );
        assert_eq!(
            to_wsl_path(&PathBuf::from(r"D:\data\runs")),
            "/mnt/d/data/runs"
        );
    }

    #[test]
    fn posix_paths_pass_through() {
        assert_eq!(to_wsl_path(&PathBuf::from("/tmp/x")), "/tmp/x");
    }

    #[test]
    fn runner_shape_spawns_wsl_launcher() {
        let r = training_runner(&PathBuf::from(r"C:\repo"));
        assert_eq!(r.program, "wsl.exe");
        assert_eq!(r.leading_args, vec!["-e", "bash", "./run-worker.sh"]);
        assert!(r.cwd.ends_with("workers/py-training") || r.cwd.ends_with(r"workers\py-training"));
    }
}
