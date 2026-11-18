// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Before anything else: the helper must never reach the single-instance check.
    if flick_lib::run_helper_if_asked() {
        return;
    }
    flick_lib::run()
}
