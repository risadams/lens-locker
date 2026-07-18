//! LumenVault Tauri app shell. Milestone 0: just a running window — no
//! commands wired to the domain crates yet. Per workplan/SPEC.md §2/§9.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
