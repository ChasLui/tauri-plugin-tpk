//! The three events the plugin emits.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

/// Download progress.
pub const DOWNLOAD_PROGRESS: &str = "tpk://download-progress";
/// State machine transitions.
pub const STATE: &str = "tpk://state";
/// Failures worth telemetry.
pub const ERROR: &str = "tpk://error";

/// Payload of [`DOWNLOAD_PROGRESS`].
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProgressPayload {
    /// Bytes fetched for the current pack.
    pub downloaded: u64,
    /// Total bytes for the current pack.
    pub total: u64,
    /// Index of the pack within the plan.
    pub pack_index: usize,
    /// How many packs the plan covers.
    pub pack_count: usize,
}

/// Payload of [`STATE`].
#[derive(Debug, Clone, Serialize)]
pub struct StatePayload {
    /// What happened: `staged`, `committed`, `rolled_back` or `reset`.
    pub pointer: String,
    /// The revision involved, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

/// Payload of [`ERROR`].
#[derive(Debug, Clone, Serialize)]
pub struct ErrorPayload {
    /// One of the frozen `E_*` codes.
    pub code: String,
    /// Detail, with any URL query already stripped.
    pub message: String,
}

/// Emit download progress.
pub fn emit_progress<R: Runtime>(app: &AppHandle<R>, progress: tpk_client::Progress) {
    let _ = app.emit(
        DOWNLOAD_PROGRESS,
        ProgressPayload {
            downloaded: progress.downloaded,
            total: progress.total,
            pack_index: progress.pack_index,
            pack_count: progress.pack_count,
        },
    );
}

/// Emit a state transition.
pub fn emit_state<R: Runtime>(app: &AppHandle<R>, pointer: &str, rev: Option<&str>) {
    let _ = app.emit(
        STATE,
        StatePayload {
            pointer: pointer.to_string(),
            rev: rev.map(str::to_string),
        },
    );
}

/// Emit a failure.
pub fn emit_error<R: Runtime>(app: &AppHandle<R>, code: &str, message: &str) {
    log::warn!("[tpk] {code}: {message}");
    let _ = app.emit(
        ERROR,
        ErrorPayload {
            code: code.to_string(),
            message: message.to_string(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_use_the_frozen_scheme() {
        for name in [DOWNLOAD_PROGRESS, STATE, ERROR] {
            assert!(name.starts_with("tpk://"), "{name}");
            // Tauri's event-name rules allow alphanumerics plus - / : _ ;
            // anything else would be rejected at emit time.
            assert!(
                name.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '/' | ':' | '_')),
                "{name}"
            );
        }
    }

    #[test]
    fn payloads_serialize_with_the_documented_field_names() {
        let json = serde_json::to_value(ProgressPayload {
            downloaded: 10,
            total: 100,
            pack_index: 0,
            pack_count: 2,
        })
        .unwrap();
        assert_eq!(json["downloaded"], 10);
        assert_eq!(json["pack_count"], 2);

        let json = serde_json::to_value(StatePayload {
            pointer: "committed".into(),
            rev: Some("rev-7".into()),
        })
        .unwrap();
        assert_eq!(json["pointer"], "committed");
        assert_eq!(json["rev"], "rev-7");

        // A state event without a revision omits the field rather than sending
        // null, so the TypeScript type can be `rev?: string`.
        let json = serde_json::to_value(StatePayload {
            pointer: "reset".into(),
            rev: None,
        })
        .unwrap();
        assert!(json.get("rev").is_none());
    }
}
