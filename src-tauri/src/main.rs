// Hydra - multi-stream downloader for macOS
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    hydra_lib::run()
}
