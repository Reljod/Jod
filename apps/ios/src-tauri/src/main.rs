// The desktop entry point. On iOS the app is started through
// `tauri::mobile_entry_point` in the library instead, which is why `run` lives
// there and this file is one line.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    jod_ios_lib::run()
}
