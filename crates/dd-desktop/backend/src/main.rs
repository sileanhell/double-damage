#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn providers_hello() -> String {
    dd_providers::hello()
}

#[tauri::command]
fn tools_hello() -> String {
    dd_tools::hello()
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![providers_hello, tools_hello])
        .run(tauri::generate_context!())
        .expect("error while running dd-desktop");
}
