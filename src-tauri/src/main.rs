// Desktop application entry; private guardian helpers run before Tauri setup.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = codewave_lib::guardian_entry() {
        std::process::exit(code);
    }
    codewave_lib::run()
}
