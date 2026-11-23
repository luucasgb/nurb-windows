// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if let Some(code) = nurb_lib::sandbox_exec() {
        std::process::exit(code);
    }
    nurb_lib::run()
}
