// Phase 0 entry point. The actual `tauri::Builder` lives in `lib.rs` so the same code can be
// loaded by mobile builds in later phases without duplicating the command registration.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    narrowmind_desktop_lib::run();
}
