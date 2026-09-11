//! The values commands return.
//!
//! Everything the caller can reasonably act on is an outcome with a `status`
//! field, not an `Err`. A frontend branching on "up to date" or "your shell is
//! too old" is doing ordinary business logic; making it catch exceptions for
//! that turns a normal path into an error path.

use serde::{Deserialize, Serialize};
use tpk_format::manifest::PackKind;

/// A pack the channel is offering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackSummary {
    /// Pack identity.
    pub id: String,
    /// What it contributes.
    pub kind: String,
    /// Display version.
    pub version: String,
    /// Monotonic ordering key.
    pub version_code: u64,
    /// Download size in bytes.
    pub size: u64,
}

impl PackSummary {
    /// Summarize a channel entry for the frontend.
    pub fn from_ref(pack: &tpk_format::channel::PackRef) -> Self {
        Self {
            id: pack.id.to_string(),
            kind: kind_name(pack.kind).to_string(),
            version: pack.version.to_string(),
            version_code: pack.version_code,
            size: pack.size,
        }
    }
}

fn kind_name(kind: PackKind) -> &'static str {
    match kind {
        PackKind::Base => "base",
        PackKind::Patch => "patch",
        PackKind::Dlc => "dlc",
        PackKind::Mod => "mod",
    }
}

/// What `check` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckOutcome {
    /// Nothing new.
    UpToDate {
        /// The watermark that was examined.
        watermark: u64,
    },
    /// An update is available.
    Available {
        /// What would be downloaded.
        packs: Vec<PackSummary>,
        /// Total bytes.
        bytes: u64,
        /// The channel's release note, truncated and not meant for end users.
        #[serde(skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
    },
    /// The channel requires a newer shell; no content applies.
    ShellRequired {
        /// The shell version required.
        min_shell: String,
    },
    /// The plugin is switched off by configuration.
    Disabled,
    /// Automatic updating has stopped after repeated rollbacks.
    Degraded {
        /// How many consecutive rollbacks were recorded.
        consecutive_rollbacks: u32,
    },
}

/// What `download` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DownloadOutcome {
    /// A revision was staged and will be tried on the next cold start.
    Staged {
        /// The revision id.
        rev: String,
        /// Bytes downloaded.
        bytes: u64,
    },
    /// Nothing to download.
    UpToDate,
    /// The channel requires a newer shell.
    ShellRequired {
        /// The shell version required.
        min_shell: String,
    },
    /// The plugin is switched off.
    Disabled,
    /// The attempt failed. Transport and disk problems land here rather than
    /// throwing, because the frontend's response is the same either way: tell
    /// the user, try again later.
    Failed {
        /// One of the frozen `E_*` codes.
        code: String,
        /// Human-readable detail, with any URL query already stripped.
        message: String,
    },
}

/// What `notify_ready` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReadyOutcome {
    /// A revision on trial was promoted.
    Committed {
        /// The revision that was promoted.
        rev: String,
    },
    /// Nothing was on trial. Calling again is harmless.
    Noop,
}

/// One loaded layer, for `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerSummary {
    /// Pack identity.
    pub id: String,
    /// What it contributes.
    pub kind: String,
    /// Monotonic ordering key.
    pub version_code: u64,
}

impl LayerSummary {
    /// Summarize a stored layer record.
    pub fn from_record(record: &tpk_store::LayerRecord) -> Self {
        Self {
            id: record.id.to_string(),
            kind: kind_name(record.kind).to_string(),
            version_code: record.version_code,
        }
    }
}

/// What `status` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// `committed` or `booting`.
    pub pointer: String,
    /// The revision currently loaded, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Layers in the loaded revision, lowest first.
    pub layers: Vec<LayerSummary>,
    /// The running shell version.
    pub shell: String,
    /// Highest watermark seen on the configured channel.
    pub watermark: u64,
    /// Whether a revision is staged for the next launch.
    pub pending: bool,
    /// Whether automatic updating has been switched off.
    pub degraded: bool,
    /// Layers that failed to load this launch, by hash.
    pub failed_layers: Vec<String>,
    /// Capabilities granted to the main window that let overlay JavaScript
    /// reach native functionality. Empty is what you want.
    pub unsafe_capabilities: Vec<String>,
    /// Whether the binary carries a usable embedded fallback.
    pub has_embedded_fallback: bool,
    /// The last recorded failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<LastError>,
}

/// A previously recorded failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastError {
    /// One of the frozen `E_*` codes.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

/// Parameters for `reset`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetOptions {
    /// Also forget the blacklist.
    ///
    /// Off by default: the blacklist is what stops a known-bad release from
    /// being installed again. Clearing it is a support action, not a routine
    /// one, which is why it needs `tpk:allow-reset`.
    #[serde(default)]
    pub clear_blacklist: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_outcomes_carry_a_snake_case_status_tag() {
        let json = serde_json::to_value(CheckOutcome::UpToDate { watermark: 42 }).unwrap();
        assert_eq!(json["status"], "up_to_date");
        assert_eq!(json["watermark"], 42);

        let json = serde_json::to_value(CheckOutcome::ShellRequired {
            min_shell: "2.4.0".into(),
        })
        .unwrap();
        assert_eq!(json["status"], "shell_required");
        assert_eq!(json["min_shell"], "2.4.0");

        assert_eq!(
            serde_json::to_value(CheckOutcome::Disabled).unwrap()["status"],
            "disabled"
        );
    }

    #[test]
    fn an_available_outcome_lists_what_would_be_downloaded() {
        let json = serde_json::to_value(CheckOutcome::Available {
            packs: vec![PackSummary {
                id: "core".into(),
                kind: "patch".into(),
                version: "1.0.3".into(),
                version_code: 10003,
                size: 180_000,
            }],
            bytes: 180_000,
            notes: Some("fixed the login page".into()),
        })
        .unwrap();

        assert_eq!(json["status"], "available");
        assert_eq!(json["bytes"], 180_000);
        assert_eq!(json["packs"][0]["id"], "core");
        assert_eq!(json["packs"][0]["kind"], "patch");
    }

    #[test]
    fn a_failed_download_is_an_outcome_not_an_exception() {
        // The frontend's response to a network failure is the same as to any
        // other "not now": tell the user, retry later.
        let json = serde_json::to_value(DownloadOutcome::Failed {
            code: "E_NETWORK".into(),
            message: "https://cdn.example.com/x.tpk?<redacted>: timeout".into(),
        })
        .unwrap();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["code"], "E_NETWORK");
        assert!(!json["message"].as_str().unwrap().contains("token="));
    }

    #[test]
    fn ready_outcomes_round_trip() {
        for outcome in [
            ReadyOutcome::Committed {
                rev: "rev-7".into(),
            },
            ReadyOutcome::Noop,
        ] {
            let json = serde_json::to_value(&outcome).unwrap();
            assert_eq!(
                serde_json::from_value::<ReadyOutcome>(json).unwrap(),
                outcome
            );
        }
    }

    #[test]
    fn reset_defaults_to_keeping_the_blacklist() {
        let opts: ResetOptions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!opts.clear_blacklist);
    }

    #[test]
    fn status_reports_the_safety_signals() {
        let status = Status {
            pointer: "booting".into(),
            rev: Some("rev-7".into()),
            layers: vec![],
            shell: "2.3.0".into(),
            watermark: 202609111500,
            pending: false,
            degraded: false,
            failed_layers: vec![],
            unsafe_capabilities: vec!["shell:allow-execute".into()],
            has_embedded_fallback: true,
            last_error: None,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["pointer"], "booting");
        assert_eq!(json["unsafe_capabilities"][0], "shell:allow-execute");
        assert_eq!(json["has_embedded_fallback"], true);
    }
}
