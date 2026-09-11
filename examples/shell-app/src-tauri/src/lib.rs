use tauri::App;

pub fn run_app(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let _ = app;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let context = tauri::generate_context!();

    // Option A: Read config from tauri.conf.json (recommended)
    let (tpk_plugin, context) = tauri_plugin_tpk::init(context)
        .expect("failed to initialize tpk plugin");

    // Option B: Explicit config
    // let (tpk_plugin, context) = tauri_plugin_tpk::init_with_config(
    //     context,
    //     tauri_plugin_tpk::HotswapConfig::new("REPLACE_WITH_YOUR_PUBKEY")
    //         .endpoint("https://example.com/api/ota/{{current_sequence}}"),
    // ).expect("failed to initialize tpk plugin");

    // Option C: Custom resolver
    // let (tpk_plugin, context) = tauri_plugin_tpk::HotswapBuilder::new("YOUR_PUBKEY")
    //     .resolver(tauri_plugin_tpk::StaticFileResolver::new(
    //         "https://cdn.example.com/ota/latest.json",
    //     ))
    //     .build(context)
    //     .expect("failed to initialize tpk plugin");

    tauri::Builder::default()
        .plugin(tpk_plugin)
        .setup(|app| run_app(app))
        .run(context)
        .expect("error while running tauri application");
}
