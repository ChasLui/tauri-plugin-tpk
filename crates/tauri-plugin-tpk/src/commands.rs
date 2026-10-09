//! The six commands the frontend can call.
//!
//! State is looked up rather than injected: when the plugin is disabled or its
//! setup degraded, nothing is managed, and an injected `State` would reject
//! with Tauri's plain "state not managed" string instead of an outcome or a
//! `{code, message}` error.

use tauri::{command, AppHandle, Manager, Runtime};

use crate::error::{Error, Result};
use crate::outcome::{CheckOutcome, DownloadOutcome, ReadyOutcome, ResetOptions, Status};
use crate::state::TpkState;

/// Poll the channel.
#[command]
pub(crate) async fn check<R: Runtime>(app: AppHandle<R>) -> Result<CheckOutcome> {
    match app.try_state::<TpkState>() {
        Some(state) => state.check(&app).await,
        None => Ok(CheckOutcome::Disabled),
    }
}

/// Download and stage whatever `check` found.
#[command]
pub(crate) async fn download<R: Runtime>(app: AppHandle<R>) -> Result<DownloadOutcome> {
    match app.try_state::<TpkState>() {
        Some(state) => state.download(&app).await,
        None => Ok(DownloadOutcome::Disabled),
    }
}

/// Acknowledge that the running revision works.
///
/// Call it once the UI has actually rendered, not at the top of your entry
/// point: what this promises is that the content is usable, and the only thing
/// that can tell is the content itself.
#[command]
pub(crate) async fn notify_ready<R: Runtime>(app: AppHandle<R>) -> Result<ReadyOutcome> {
    match app.try_state::<TpkState>() {
        Some(state) => state.notify_ready(&app),
        None => Ok(ReadyOutcome::Noop),
    }
}

/// Report what is loaded and what is pending.
#[command]
pub(crate) async fn status<R: Runtime>(app: AppHandle<R>) -> Result<Status> {
    app.try_state::<TpkState>()
        .ok_or(Error::Disabled)?
        .status(&app)
}

/// Forget downloaded content. Requires `tpk:allow-reset`.
#[command]
pub(crate) async fn reset<R: Runtime>(
    app: AppHandle<R>,
    options: Option<ResetOptions>,
) -> Result<Status> {
    app.try_state::<TpkState>()
        .ok_or(Error::Disabled)?
        .reset(&app, options.unwrap_or_default())
}

/// Enable or disable a mod layer. Requires `tpk:allow-mods`.
///
/// Present so the command name is reserved, and refused so the capability
/// cannot become a live code path by accident. Mod layers are desktop-only and
/// nothing loads them today; see `spec/tpk-v1.md` §11.1.
#[command]
pub(crate) async fn set_mod_enabled<R: Runtime>(
    app: AppHandle<R>,
    _id: String,
    _enabled: bool,
) -> Result<()> {
    app.try_state::<TpkState>().ok_or(Error::Disabled)?;
    Err(Error::Config("mod layers are not implemented".into()))
}
