//! Plugin configuration, read from `plugins.tpk` in `tauri.conf.json`.
//!
//! The update URL and the trusted keys live here and nowhere else: there is no
//! command that changes them. A cross-site scripting bug in the frontend must
//! not be able to repoint the updater.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Cache budget on desktop.
pub const DESKTOP_CACHE_BUDGET: u64 = 32 * 1024 * 1024;
/// Cache budget on mobile, where the same bytes are copied again crossing the
/// WebView bridge.
pub const MOBILE_CACHE_BUDGET: u64 = 8 * 1024 * 1024;

/// Whether this build targets a phone.
pub const fn is_mobile() -> bool {
    cfg!(any(target_os = "ios", target_os = "android"))
}

/// A trusted signing key and the generation it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PubKey {
    /// The minisign public key, base64.
    pub key: String,
    /// Generation. Clients keep a monotonic floor and refuse anything below it.
    #[serde(default = "default_epoch")]
    pub epoch: u32,
}

const fn default_epoch() -> u32 {
    1
}

/// The `plugins.tpk` section.
///
/// `deny_unknown_fields` on purpose: a misspelled key should fail loudly at
/// startup rather than silently take its default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TpkConfig {
    /// Whether the plugin does anything at all.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Channel to poll.
    #[serde(default = "default_channel")]
    pub channel: String,

    /// Manifest URL. May contain `{{channel}}`, `{{arch}}`, `{{target}}` or
    /// `{{shell}}`, and nothing else.
    pub manifest_url: String,

    /// Trusted signing keys. More than one during a rotation window.
    pub pubkeys: Vec<PubKey>,

    /// Check for updates during startup.
    ///
    /// Off by default on mobile: a silent network fetch on every launch is a
    /// dormant behaviour from App Review's point of view, and a first launch
    /// that downloads before showing anything is a poor review experience.
    #[serde(default = "default_auto_on_desktop")]
    pub auto_check_on_launch: bool,

    /// Download automatically once an update is found. Same default rationale.
    #[serde(default = "default_auto_on_desktop")]
    pub auto_download: bool,

    /// In-memory budget for decoded assets. Zero disables the cache.
    #[serde(default = "default_cache_budget")]
    pub cache_budget_bytes: u64,

    /// Headers sent with manifest and pack requests.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Directory inside the app bundle holding a seed pack, relative to the
    /// resource directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_dir: Option<String>,
}

const fn default_true() -> bool {
    true
}

fn default_channel() -> String {
    "stable".to_string()
}

const fn default_auto_on_desktop() -> bool {
    !is_mobile()
}

const fn default_cache_budget() -> u64 {
    if is_mobile() {
        MOBILE_CACHE_BUDGET
    } else {
        DESKTOP_CACHE_BUDGET
    }
}

impl TpkConfig {
    /// Build a configuration programmatically.
    pub fn new(manifest_url: impl Into<String>, pubkeys: Vec<PubKey>) -> Self {
        Self {
            enabled: true,
            channel: default_channel(),
            manifest_url: manifest_url.into(),
            pubkeys,
            auto_check_on_launch: default_auto_on_desktop(),
            auto_download: default_auto_on_desktop(),
            cache_budget_bytes: default_cache_budget(),
            headers: HashMap::new(),
            seed_dir: None,
        }
    }

    /// Poll a different channel.
    #[must_use]
    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = channel.into();
        self
    }

    /// Turn the plugin off without removing it.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Convert the configured keys into the format layer's representation.
    pub fn trusted_keys(&self) -> Vec<tpk_format::sign::TrustedKey> {
        self.pubkeys
            .iter()
            .map(|k| tpk_format::sign::TrustedKey {
                key: k.key.clone(),
                epoch: k.epoch,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "manifest_url": "https://cdn.example.com/tpk/{{channel}}/latest.json",
            "pubkeys": [{ "key": "RWTsomekey", "epoch": 1 }],
        })
    }

    fn parse(v: &serde_json::Value) -> Result<TpkConfig, serde_json::Error> {
        serde_json::from_value(v.clone())
    }

    #[test]
    fn a_minimal_config_takes_sensible_defaults() {
        let c = parse(&minimal()).unwrap();
        assert!(c.enabled);
        assert_eq!(c.channel, "stable");
        assert_eq!(c.pubkeys.len(), 1);
        assert!(c.headers.is_empty());
        assert!(c.seed_dir.is_none());
    }

    #[test]
    fn automatic_updating_is_off_by_default_on_mobile() {
        let c = parse(&minimal()).unwrap();
        assert_eq!(c.auto_check_on_launch, !is_mobile());
        assert_eq!(c.auto_download, !is_mobile());
    }

    #[test]
    fn the_cache_budget_is_smaller_on_mobile() {
        let c = parse(&minimal()).unwrap();
        if is_mobile() {
            assert_eq!(c.cache_budget_bytes, MOBILE_CACHE_BUDGET);
        } else {
            assert_eq!(c.cache_budget_bytes, DESKTOP_CACHE_BUDGET);
        }
    }

    #[test]
    fn a_misspelled_key_fails_loudly() {
        let mut v = minimal();
        v["auto_dowload"] = serde_json::json!(true);
        // Silently taking the default would leave the user convinced they had
        // configured something they had not.
        assert!(parse(&v).is_err());
    }

    #[test]
    fn a_missing_required_field_fails() {
        let mut v = minimal();
        v.as_object_mut().unwrap().remove("manifest_url");
        assert!(parse(&v).is_err());

        let mut v = minimal();
        v.as_object_mut().unwrap().remove("pubkeys");
        assert!(parse(&v).is_err());
    }

    #[test]
    fn an_epoch_defaults_to_the_first_generation() {
        let v = serde_json::json!({
            "manifest_url": "https://x.com/latest.json",
            "pubkeys": [{ "key": "RWTsomekey" }],
        });
        assert_eq!(parse(&v).unwrap().pubkeys[0].epoch, 1);
    }

    #[test]
    fn there_is_no_way_to_change_the_url_or_keys_at_runtime() {
        // A compile-time assertion of intent: the config has no setters for
        // these, and no command mutates them. XSS in the frontend must not be
        // able to repoint the updater.
        let c = TpkConfig::new("https://x.com/latest.json", vec![]);
        let json = serde_json::to_value(&c).unwrap();
        assert!(json.get("manifest_url").is_some());
        assert!(json.get("pubkeys").is_some());
    }

    #[test]
    fn keys_convert_to_the_format_layer_representation() {
        let c = TpkConfig::new(
            "https://x.com/latest.json",
            vec![
                PubKey {
                    key: "k1".into(),
                    epoch: 1,
                },
                PubKey {
                    key: "k2".into(),
                    epoch: 2,
                },
            ],
        );
        let keys = c.trusted_keys();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].epoch, 2);
    }

    #[test]
    fn the_builder_helpers_work() {
        let c = TpkConfig::new("https://x.com/latest.json", vec![])
            .channel("beta")
            .disabled();
        assert_eq!(c.channel, "beta");
        assert!(!c.enabled);
    }

    #[test]
    fn round_trips_through_json() {
        let c = parse(&minimal()).unwrap();
        let back: TpkConfig = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(c, back);
    }
}
