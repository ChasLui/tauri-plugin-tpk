#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    // Config-created windows have no response hook. Keep their complete
    // configuration, but create them ourselves below so tombstones return 404
    // instead of Tauri's ordinary SPA index fallback.
    let windows: Vec<_> = context
        .config()
        .app
        .windows
        .iter()
        .filter(|window| window.create)
        .cloned()
        .collect();
    for window in &mut context.config_mut().app.windows {
        window.create = false;
    }

    // `attach` swaps the asset provider before the app is built. It resolves no
    // paths and opens no files — on Android the data directory is not reachable
    // this early — so everything stateful happens in the plugin's setup hook,
    // which still runs before any window exists.
    let tpk = tauri_plugin_tpk::attach(&mut context);

    tauri::Builder::default()
        // Register tpk first so its setup runs before anything that might want
        // to read assets.
        .plugin(tauri_plugin_tpk::init(tpk.clone()))
        .setup(move |app| {
            for config in &windows {
                let handle = tpk.clone();
                tauri::WebviewWindowBuilder::from_config(app.handle(), config)?
                    .on_web_resource_request(move |request, response| {
                        handle.filter_web_resource_response(request.uri(), response);
                    })
                    .build()?;
            }
            Ok(())
        })
        .run(context)
        .expect("error while running tauri application");
}
