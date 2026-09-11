#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();

    // `attach` swaps the asset provider before the app is built. It resolves no
    // paths and opens no files — on Android the data directory is not reachable
    // this early — so everything stateful happens in the plugin's setup hook,
    // which still runs before any window exists.
    let tpk = tauri_plugin_tpk::attach(&mut context);

    tauri::Builder::default()
        // Register tpk first so its setup runs before anything that might want
        // to read assets.
        .plugin(tauri_plugin_tpk::init(tpk))
        .run(context)
        .expect("error while running tauri application");
}
