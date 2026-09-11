//! The six commands the frontend can call.

use tauri::{command, AppHandle, Runtime, State};

use crate::error::Result;
use crate::outcome::{CheckOutcome, DownloadOutcome, ReadyOutcome, ResetOptions, Status};
use crate::state::TpkState;

/// Poll the channel.
#[command]
pub(crate) async fn check<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, TpkState>,
) -> Result<CheckOutcome> {
    state.check(&app).await
}

/// Download and stage whatever `check` found.
#[command]
pub(crate) async fn download<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, TpkState>,
) -> Result<DownloadOutcome> {
    state.download(&app).await
}

/// Acknowledge that the running revision works.
///
/// Call it once the UI has actually rendered, not at the top of your entry
/// point: what this promises is that the content is usable, and the only thing
/// that can tell is the content itself.
#[command]
pub(crate) async fn notify_ready<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, TpkState>,
) -> Result<ReadyOutcome> {
    state.notify_ready(&app)
}

/// Report what is loaded and what is pending.
#[command]
pub(crate) async fn status<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, TpkState>,
) -> Result<Status> {
    state.status(&app)
}

/// Forget downloaded content. Requires `tpk:allow-reset`.
#[command]
pub(crate) async fn reset<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, TpkState>,
    options: Option<ResetOptions>,
) -> Result<Status> {
    state.reset(&app, options.unwrap_or_default())
}

/// Enable or disable a mod layer. Requires `tpk:allow-mods`.
///
/// Present so the command name is reserved, and refused so the capability
/// cannot become a live code path by accident. Mod layers are desktop-only and
/// nothing loads them today; see `spec/tpk-v1.md` A.4.
#[command]
pub(crate) async fn set_mod_enabled<R: Runtime>(
    _app: AppHandle<R>,
    _state: State<'_, TpkState>,
    _id: String,
    _enabled: bool,
) -> Result<()> {
    Err(crate::error::Error::Config(
        "mod layers are not implemented".into(),
    ))
}
