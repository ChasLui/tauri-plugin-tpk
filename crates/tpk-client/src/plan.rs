//! Deciding what to download.
//!
//! A pure function over a signed channel manifest and what the device already
//! has. Everything that decides whether a pack is allowed lives here, so the
//! rules can be tested without a disk or a network.

use std::collections::HashMap;

use tpk_format::channel::{ChannelManifest, PackRef};
use tpk_format::manifest::{PackId, PackKind, Sha256Hex};

/// What the device already has, and who it is.
#[derive(Debug, Clone)]
pub struct PlanContext {
    /// The running shell version.
    pub shell_version: semver::Version,
    /// Stable per-installation identifier, for rollout bucketing.
    pub install_id: String,
    /// Highest `version_code` currently installed, per pack id.
    ///
    /// This is the downgrade floor, and it is derived from the layers actually
    /// on disk rather than from a counter — deleting `state.json` must not lower
    /// it.
    pub installed: HashMap<PackId, u64>,
}

/// Why a pack was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// The shell is below the pack's `min_shell`.
    ShellTooOld {
        /// What the pack requires.
        required: semver::Version,
    },
    /// Its `version_code` is not above what is installed.
    NotNewer {
        /// What the device has.
        installed: u64,
    },
    /// Its parent is not the version the device has.
    ParentMissing {
        /// The `version_code` the patch expects underneath it.
        expected: u64,
    },
    /// The device is outside this pack's rollout bucket.
    NotInRollout {
        /// The advertised percentage.
        rollout: u8,
    },
    /// The pack is blacklisted.
    Blacklisted,
    /// Mod packs never arrive over a channel.
    ModOverChannel,
}

/// What a pack was left out for, with its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPack {
    /// Which pack.
    pub id: PackId,
    /// Its version code.
    pub version_code: u64,
    /// Why.
    pub reason: Skipped,
}

/// The outcome of planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to do.
    UpToDate {
        /// Packs that were considered and rejected, for diagnostics.
        skipped: Vec<SkippedPack>,
    },
    /// The channel demands a newer shell; no content applies at all.
    ShellRequired {
        /// The shell version the channel requires.
        min_shell: semver::Version,
    },
    /// Packs to fetch, lowest layer first.
    Apply {
        /// What to download.
        packs: Vec<PackRef>,
        /// Total bytes.
        bytes: u64,
        /// Packs that were considered and rejected.
        skipped: Vec<SkippedPack>,
    },
}

impl Plan {
    /// Whether anything needs downloading.
    pub fn has_work(&self) -> bool {
        matches!(self, Self::Apply { .. })
    }

    /// Packs that were left out.
    pub fn skipped(&self) -> &[SkippedPack] {
        match self {
            Self::UpToDate { skipped } | Self::Apply { skipped, .. } => skipped,
            Self::ShellRequired { .. } => &[],
        }
    }
}

/// Decide what to fetch from a verified channel manifest.
///
/// `is_blacklisted` is a callback rather than a dependency so this crate stays
/// independent of the store, which keeps `plan` a pure function.
pub fn plan(
    manifest: &ChannelManifest,
    ctx: &PlanContext,
    is_blacklisted: &dyn Fn(&Sha256Hex, &PackId, u64) -> bool,
) -> Plan {
    // `force_shell` stops everything, hotfixes included. That bluntness is the
    // point: it is for shells that should receive no content at all.
    if let Some(required) = &manifest.force_shell {
        if ctx.shell_version < *required {
            return Plan::ShellRequired {
                min_shell: required.clone(),
            };
        }
    }
    if let Some(required) = &manifest.min_shell {
        if ctx.shell_version < *required {
            return Plan::ShellRequired {
                min_shell: required.clone(),
            };
        }
    }

    let mut chosen: Vec<PackRef> = Vec::new();
    let mut skipped: Vec<SkippedPack> = Vec::new();
    // Tracks what each id would be at after applying what is already chosen, so
    // a base and the patches above it can be planned in one pass.
    let mut projected: HashMap<PackId, u64> = ctx.installed.clone();

    let mut candidates: Vec<&PackRef> = manifest.packs.iter().collect();
    // Lowest layer first, then by version_code: a base has to be decided before
    // the patches that sit on it.
    candidates.sort_by_key(|p| (layer_rank(p.kind), p.version_code));

    for pack in candidates {
        let note = |reason: Skipped| SkippedPack {
            id: pack.id.clone(),
            version_code: pack.version_code,
            reason,
        };

        // A mod never arrives over a channel, whatever `allow_mods` says: an
        // unsigned layer sits above everything and inherits the main window's
        // capabilities.
        if pack.kind == PackKind::Mod {
            skipped.push(note(Skipped::ModOverChannel));
            continue;
        }
        if is_blacklisted(&pack.sha256, &pack.id, pack.version_code) {
            skipped.push(note(Skipped::Blacklisted));
            continue;
        }

        // Only the channel-level `min_shell` is checkable here: a channel entry
        // carries no shell range of its own. The pack's own `min_shell` /
        // `max_shell` are inside the signed manifest and are enforced when the
        // downloaded bytes are verified, before anything is staged.
        if let Some(required) = &manifest.min_shell {
            if ctx.shell_version < *required {
                skipped.push(note(Skipped::ShellTooOld {
                    required: required.clone(),
                }));
                continue;
            }
        }

        let current = projected.get(&pack.id).copied().unwrap_or(0);
        match pack.kind {
            PackKind::Patch => {
                // A patch is only usable if the device is exactly at its parent.
                let expected = pack.parent_version_code.unwrap_or(0);
                if current != expected {
                    skipped.push(note(Skipped::ParentMissing { expected }));
                    continue;
                }
            }
            _ => {
                // Monotonic version codes are the downgrade defence. A publisher
                // rolling back a release produces a perfectly signed manifest
                // that would otherwise push users back onto known-bad content.
                if pack.version_code <= current {
                    skipped.push(note(Skipped::NotNewer { installed: current }));
                    continue;
                }
            }
        }

        if !in_rollout(&ctx.install_id, pack) {
            skipped.push(note(Skipped::NotInRollout {
                rollout: pack.rollout,
            }));
            continue;
        }

        projected.insert(pack.id.clone(), pack.version_code);
        chosen.push(pack.clone());
    }

    if chosen.is_empty() {
        Plan::UpToDate { skipped }
    } else {
        let bytes = chosen.iter().map(|p| p.size).sum();
        Plan::Apply {
            packs: chosen,
            bytes,
            skipped,
        }
    }
}

/// Whether this device falls inside a pack's staged rollout.
///
/// The bucket is derived from the install id, the pack id and its version code.
/// Including the version code reshuffles for every release — otherwise the same
/// unlucky devices would receive every canary.
pub fn in_rollout(install_id: &str, pack: &PackRef) -> bool {
    if pack.rollout >= 100 {
        return true;
    }
    let digest = tpk_format::sign::sha256_hex(
        format!("{install_id}:{}:{}", pack.id, pack.version_code).as_bytes(),
    );
    let bytes = digest.as_bytes();
    let value = u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    (value % 100) < u64::from(pack.rollout)
}

fn layer_rank(kind: PackKind) -> u8 {
    match kind {
        PackKind::Base => 0,
        PackKind::Patch => 1,
        PackKind::Dlc => 2,
        PackKind::Mod => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::sign::sha256_hex;

    fn pack(kind: PackKind, code: u64, parent: Option<u64>) -> PackRef {
        PackRef {
            id: PackId::parse("core").unwrap(),
            kind,
            version: "1.0.0".parse().unwrap(),
            version_code: code,
            parent_version_code: parent,
            url: format!("https://cdn.example.com/tpk/core/{code}.tpk"),
            size: 1000,
            sha256: sha256_hex(format!("pack-{code}").as_bytes()),
            optional: false,
            rollout: 100,
        }
    }

    fn manifest(packs: Vec<PackRef>) -> ChannelManifest {
        ChannelManifest {
            spec: tpk_format::channel::CHANNEL_SPEC_TAG.to_string(),
            channel: "stable".to_string(),
            published_at: "2026-09-11T15:00:00Z".to_string(),
            watermark: 202609111500,
            key_epoch: 1,
            min_shell: None,
            force_shell: None,
            notes: None,
            packs,
        }
    }

    fn ctx(shell: &str, installed: &[(&str, u64)]) -> PlanContext {
        PlanContext {
            shell_version: shell.parse().unwrap(),
            install_id: "8f14e45f-ceea-467a-9a2f-0b8f5c8b3a21".to_string(),
            installed: installed
                .iter()
                .map(|(id, code)| (PackId::parse(id).unwrap(), *code))
                .collect(),
        }
    }

    fn nothing_blacklisted(_: &Sha256Hex, _: &PackId, _: u64) -> bool {
        false
    }

    fn run(m: &ChannelManifest, c: &PlanContext) -> Plan {
        plan(m, c, &nothing_blacklisted)
    }

    #[test]
    fn a_fresh_device_takes_the_base() {
        let m = manifest(vec![pack(PackKind::Base, 100, None)]);
        let Plan::Apply { packs, bytes, .. } = run(&m, &ctx("2.3.0", &[])) else {
            panic!("expected work");
        };
        assert_eq!(packs.len(), 1);
        assert_eq!(bytes, 1000);
    }

    #[test]
    fn an_up_to_date_device_does_nothing() {
        let m = manifest(vec![pack(PackKind::Base, 100, None)]);
        assert!(!run(&m, &ctx("2.3.0", &[("core", 100)])).has_work());
    }

    #[test]
    fn a_lower_version_code_is_refused() {
        // A publisher rolling back produces a perfectly signed manifest; the
        // monotonic check is what stops it from pushing users onto old content.
        let m = manifest(vec![pack(PackKind::Base, 50, None)]);
        let outcome = run(&m, &ctx("2.3.0", &[("core", 100)]));
        assert!(!outcome.has_work());
        assert_eq!(
            outcome.skipped()[0].reason,
            Skipped::NotNewer { installed: 100 }
        );
    }

    #[test]
    fn an_equal_version_code_is_refused() {
        let m = manifest(vec![pack(PackKind::Base, 100, None)]);
        assert!(!run(&m, &ctx("2.3.0", &[("core", 100)])).has_work());
    }

    #[test]
    fn a_patch_needs_its_exact_parent() {
        let m = manifest(vec![pack(PackKind::Patch, 200, Some(100))]);

        // Device is at 100: the patch applies.
        assert!(run(&m, &ctx("2.3.0", &[("core", 100)])).has_work());

        // Device is at 150: this patch rebuilds from the wrong base.
        let outcome = run(&m, &ctx("2.3.0", &[("core", 150)]));
        assert!(!outcome.has_work());
        assert_eq!(
            outcome.skipped()[0].reason,
            Skipped::ParentMissing { expected: 100 }
        );
    }

    #[test]
    fn a_base_and_its_patch_are_planned_together() {
        let m = manifest(vec![
            pack(PackKind::Patch, 200, Some(100)),
            pack(PackKind::Base, 100, None),
        ]);
        let Plan::Apply { packs, .. } = run(&m, &ctx("2.3.0", &[])) else {
            panic!("expected work");
        };
        // The base has to be decided before the patch that sits on it, whatever
        // order the manifest lists them in.
        assert_eq!(packs.len(), 2);
        assert_eq!(packs[0].kind, PackKind::Base);
        assert_eq!(packs[1].kind, PackKind::Patch);
    }

    #[test]
    fn an_existing_base_is_not_downloaded_again() {
        let m = manifest(vec![
            pack(PackKind::Base, 100, None),
            pack(PackKind::Patch, 200, Some(100)),
        ]);
        let Plan::Apply { packs, .. } = run(&m, &ctx("2.3.0", &[("core", 100)])) else {
            panic!("expected work");
        };
        assert_eq!(packs.len(), 1, "only the patch");
        assert_eq!(packs[0].kind, PackKind::Patch);
    }

    #[test]
    fn a_chain_of_patches_applies_in_order() {
        let m = manifest(vec![
            pack(PackKind::Patch, 300, Some(200)),
            pack(PackKind::Patch, 200, Some(100)),
            pack(PackKind::Base, 100, None),
        ]);
        let Plan::Apply { packs, .. } = run(&m, &ctx("2.3.0", &[])) else {
            panic!("expected work");
        };
        assert_eq!(
            packs.iter().map(|p| p.version_code).collect::<Vec<_>>(),
            [100, 200, 300]
        );
    }

    #[test]
    fn force_shell_blocks_everything_including_hotfixes() {
        let mut m = manifest(vec![pack(PackKind::Base, 100, None)]);
        m.force_shell = Some("3.0.0".parse().unwrap());

        assert!(matches!(
            run(&m, &ctx("2.3.0", &[])),
            Plan::ShellRequired { .. }
        ));
        // A shell at or above the bar is unaffected.
        assert!(run(&m, &ctx("3.0.0", &[])).has_work());
    }

    #[test]
    fn min_shell_blocks_the_whole_manifest() {
        let mut m = manifest(vec![pack(PackKind::Base, 100, None)]);
        m.min_shell = Some("2.4.0".parse().unwrap());

        let Plan::ShellRequired { min_shell } = run(&m, &ctx("2.3.0", &[])) else {
            panic!("expected a shell requirement");
        };
        assert_eq!(min_shell, "2.4.0".parse::<semver::Version>().unwrap());
        assert!(run(&m, &ctx("2.4.0", &[])).has_work());
    }

    #[test]
    fn a_mod_never_arrives_over_a_channel() {
        let m = manifest(vec![pack(PackKind::Mod, 100, None)]);
        // Unconditional: an unsigned layer sits above everything else and its
        // JS inherits the main window's capabilities.
        let outcome = run(&m, &ctx("2.3.0", &[]));
        assert!(!outcome.has_work());
        assert_eq!(outcome.skipped()[0].reason, Skipped::ModOverChannel);
    }

    #[test]
    fn a_blacklisted_pack_is_skipped() {
        let m = manifest(vec![pack(PackKind::Base, 100, None)]);
        let outcome = plan(&m, &ctx("2.3.0", &[]), &|_, _, _| true);
        assert!(!outcome.has_work());
        assert_eq!(outcome.skipped()[0].reason, Skipped::Blacklisted);
    }

    #[test]
    fn a_dlc_is_independent_of_the_core_chain() {
        let mut dlc = pack(PackKind::Dlc, 500, None);
        dlc.id = PackId::parse("maps").unwrap();
        let m = manifest(vec![pack(PackKind::Base, 100, None), dlc]);

        let Plan::Apply { packs, .. } = run(&m, &ctx("2.3.0", &[])) else {
            panic!("expected work");
        };
        assert_eq!(packs.len(), 2);
        assert_eq!(packs[1].kind, PackKind::Dlc, "dlc stacks above the base");
    }

    #[test]
    fn a_full_rollout_reaches_every_device() {
        let p = pack(PackKind::Base, 100, None);
        for i in 0..200 {
            assert!(in_rollout(&format!("device-{i}"), &p));
        }
    }

    #[test]
    fn a_partial_rollout_splits_devices_roughly_as_advertised() {
        let mut p = pack(PackKind::Base, 100, None);
        p.rollout = 25;
        let included = (0..2000)
            .filter(|i| in_rollout(&format!("device-{i}"), &p))
            .count();
        // Hashing is not a perfect splitter; a wide band still catches a bucket
        // that is wildly off.
        assert!(
            (400..600).contains(&included),
            "expected roughly 500 of 2000, got {included}"
        );
    }

    #[test]
    fn the_bucket_is_stable_for_one_device_and_release() {
        let p = pack(PackKind::Base, 100, None);
        let mut p25 = p.clone();
        p25.rollout = 25;
        let first = in_rollout("a-stable-device", &p25);
        for _ in 0..10 {
            assert_eq!(in_rollout("a-stable-device", &p25), first);
        }
    }

    #[test]
    fn every_release_reshuffles_the_buckets() {
        // Otherwise the same unlucky devices would get every single canary.
        let mut a = pack(PackKind::Base, 100, None);
        a.rollout = 50;
        let mut b = pack(PackKind::Base, 200, None);
        b.rollout = 50;

        let differing = (0..500)
            .filter(|i| {
                let id = format!("device-{i}");
                in_rollout(&id, &a) != in_rollout(&id, &b)
            })
            .count();
        assert!(
            differing > 100,
            "buckets barely moved between releases: {differing}"
        );
    }

    #[test]
    fn a_device_outside_the_bucket_is_reported_as_such() {
        let mut p = pack(PackKind::Base, 100, None);
        p.rollout = 1;
        let m = manifest(vec![p]);
        // Find a device that is excluded, then check the reason is recorded.
        let excluded = (0..500)
            .map(|i| format!("device-{i}"))
            .find(|id| {
                let mut c = ctx("2.3.0", &[]);
                c.install_id = id.clone();
                !run(&m, &c).has_work()
            })
            .expect("a 1% rollout should exclude someone");

        let mut c = ctx("2.3.0", &[]);
        c.install_id = excluded;
        assert_eq!(
            run(&m, &c).skipped()[0].reason,
            Skipped::NotInRollout { rollout: 1 }
        );
    }

    #[test]
    fn an_empty_manifest_is_up_to_date() {
        assert!(!run(&manifest(vec![]), &ctx("2.3.0", &[])).has_work());
    }
}
