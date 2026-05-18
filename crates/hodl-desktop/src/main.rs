// Hide the Windows console in release builds. No-op on Linux/macOS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    hodl_desktop_lib::run();
}
