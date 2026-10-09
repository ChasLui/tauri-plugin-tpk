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
    /// Derived from the layers actually on disk rather than from a counter, and
    /// it excludes blacklisted layers: this answers "what can a patch stack on",
    /// which is a question about the stack as it really is.
    pub installed: HashMap<PackId, u64>,
    /// Highest `version_code` ever committed, per pack id.
    ///
    /// Answers the other question — "how far back may this id go" — which
    /// [`Self::installed`] cannot, because condemning a layer removes it from
    /// the stack without unmaking the fact that the device ran that version.
    /// Persisted by the store and never lowered, `reset` included.
    pub version_floor: HashMap<PackId, u64>,
}

/// Why a pack was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// The shell is below the pack's `min_shell`.
    ShellTooOld {
        /// What the pack requires.
        required: semver::Version,
    },
    /// The shell is above the pack's `max_shell`.
    ShellTooNew {
        /// The highest shell the pack supports.
        supported: semver::Version,
    },
    /// Its `version_code` is not above what is installed.
    NotNewer {
        /// What the device has.
        installed: u64,
    },
    /// Its `version_code` is below this id's persistent floor: the device has
    /// run a newer version, even if that version is no longer installed.
    ///
    /// Separate from [`Self::NotNewer`] because they mean different things to
    /// whoever reads `plan.skipped()`. `NotNewer` is the ordinary "this device
    /// is already up to date"; this one is the anti-rollback defence firing,
    /// which is worth noticing.
    Downgrade {
        /// The lowest `version_code` this id will accept.
        floor: u64,
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
    /// A newer base or DLC of the same id is itself eligible, so this pack (or
    /// the patch stacked on an older one) would be downloaded only to be
    /// replaced.
    Superseded {
        /// The `version_code` of the eligible newer pack.
        by: u64,
    },
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

    // Staging a base or DLC replaces every same-id layer, so when a newer one is
    // itself eligible, the older chain beneath it would be downloaded for nothing.
    let mut newest: HashMap<PackId, u64> = HashMap::new();
    for pack in &manifest.packs {
        if pack.kind == PackKind::Patch {
            continue;
        }
        let installed = ctx.installed.get(&pack.id).copied().unwrap_or(0);
        if check(manifest, ctx, is_blacklisted, pack, installed, None).is_ok() {
            let code = newest.entry(pack.id.clone()).or_insert(0);
            *code = (*code).max(pack.version_code);
        }
    }

    let mut candidates: Vec<&PackRef> = manifest.packs.iter().collect();
    // Lowest layer first, then by version_code: a base has to be decided before
    // the patches that sit on it.
    candidates.sort_by_key(|p| (layer_rank(p.kind), p.version_code));

    for pack in candidates {
        let current = projected.get(&pack.id).copied().unwrap_or(0);
        let newest = newest.get(&pack.id).copied();
        if let Err(reason) = check(manifest, ctx, is_blacklisted, pack, current, newest) {
            skipped.push(SkippedPack {
                id: pack.id.clone(),
                version_code: pack.version_code,
                reason,
            });
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

/// Every rule a single pack has to pass, in the order they are reported.
///
/// `current` is the version the pack's id would be at when it is applied.
/// `newest` is the highest eligible base/DLC `version_code` for that id; the
/// pre-pass that computes it passes `None`, so both share exactly these checks.
fn check(
    manifest: &ChannelManifest,
    ctx: &PlanContext,
    is_blacklisted: &dyn Fn(&Sha256Hex, &PackId, u64) -> bool,
    pack: &PackRef,
    current: u64,
    newest: Option<u64>,
) -> Result<(), Skipped> {
    // A mod never arrives over a channel, whatever `allow_mods` says: an
    // unsigned layer sits above everything and inherits the main window's
    // capabilities. In an App Store build the channel parser has already
    // dropped the entry, so there is nothing left to reject here.
    #[cfg(not(app_store))]
    if pack.kind == PackKind::Mod {
        return Err(Skipped::ModOverChannel);
    }
    if is_blacklisted(&pack.sha256, &pack.id, pack.version_code) {
        return Err(Skipped::Blacklisted);
    }

    // The channel-level `min_shell`, then the pack's own range as the channel
    // entry repeats it. A pack the shell cannot stage would fail the whole
    // staging batch on every check. The signed manifest's range is enforced
    // again at staging, for channels that do not carry it.
    for required in [&manifest.min_shell, &pack.min_shell].into_iter().flatten() {
        if ctx.shell_version < *required {
            return Err(Skipped::ShellTooOld {
                required: required.clone(),
            });
        }
    }
    if let Some(supported) = &pack.max_shell {
        if ctx.shell_version > *supported {
            return Err(Skipped::ShellTooNew {
                supported: supported.clone(),
            });
        }
    }

    // A patch stacks on its parent, so it is superseded along with that parent.
    let floor = match pack.kind {
        PackKind::Patch => pack.parent_version_code.unwrap_or(0),
        _ => pack.version_code,
    };
    if let Some(by) = newest.filter(|&by| floor < by) {
        return Err(Skipped::Superseded { by });
    }

    match pack.kind {
        PackKind::Patch => {
            // A patch is only usable if the device is exactly at its parent.
            let expected = pack.parent_version_code.unwrap_or(0);
            if current != expected {
                return Err(Skipped::ParentMissing { expected });
            }
        }
        _ => {
            // Two rules that look like one. "Already have it" is equality
            // against what is really installed. "Never go backwards" is strict
            // against the persistent floor, which outlives a blacklisted layer
            // and a `reset` — strict, so re-fetching the version the device is
            // already on stays possible after a reset, while anything older
            // stays refused. A publisher rolling back a release produces a
            // perfectly signed manifest, and this is what stops it.
            if pack.version_code <= current {
                return Err(Skipped::NotNewer { installed: current });
            }
            let floor = ctx.version_floor.get(&pack.id).copied().unwrap_or(0);
            if pack.version_code < floor {
                return Err(Skipped::Downgrade { floor });
            }
        }
    }

    if !in_rollout(&ctx.install_id, pack) {
        return Err(Skipped::NotInRollout {
            rollout: pack.rollout,
        });
    }
    Ok(())
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
        #[cfg(not(app_store))]
        PackKind::Dlc => 2,
        #[cfg(not(app_store))]
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
            min_shell: None,
            max_shell: None,
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
            version_floor: HashMap::new(),
        }
    }

    /// A context whose id is condemned — nothing installed, but the floor
    /// remembers how far the device got.
    fn ctx_with_floor(shell: &str, installed: &[(&str, u64)], floor: u64) -> PlanContext {
        let mut c = ctx(shell, installed);
        c.version_floor
            .insert(PackId::parse("core").unwrap(), floor);
        c
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
    fn a_device_partway_up_a_chain_takes_only_what_it_lacks() {
        let m = manifest(vec![
            pack(PackKind::Base, 100, None),
            pack(PackKind::Patch, 200, Some(100)),
            pack(PackKind::Patch, 300, Some(200)),
        ]);
        let codes = |installed: u64| match run(&m, &ctx("2.3.0", &[("core", installed)])) {
            Plan::Apply { packs, .. } => packs.iter().map(|p| p.version_code).collect::<Vec<_>>(),
            other => panic!("expected work at {installed}, got {other:?}"),
        };
        assert_eq!(codes(100), [200, 300], "a device that missed a patch");
        assert_eq!(codes(200), [300]);
    }

    #[test]
    fn a_partial_base_rollout_leaves_the_old_chain_reachable() {
        let mut new_base = pack(PackKind::Base, 300, None);
        new_base.rollout = 10;
        let m = manifest(vec![
            pack(PackKind::Base, 100, None),
            pack(PackKind::Patch, 200, Some(100)),
            new_base.clone(),
        ]);
        let outside = (0..500)
            .map(|i| format!("device-{i}"))
            .find(|id| !in_rollout(id, &new_base))
            .expect("a 10% rollout should exclude someone");
        let inside = (0..500)
            .map(|i| format!("device-{i}"))
            .find(|id| in_rollout(id, &new_base))
            .expect("a 10% rollout should include someone");
        let codes = |install_id: &str, installed: &[(&str, u64)]| {
            let mut c = ctx("2.3.0", installed);
            c.install_id = install_id.to_string();
            match run(&m, &c) {
                Plan::Apply { packs, .. } => packs.iter().map(|p| p.version_code).collect(),
                Plan::UpToDate { .. } => Vec::new(),
                other => panic!("unexpected {other:?}"),
            }
        };

        // Outside the bucket: a fresh device still converges on the old chain,
        // and a device already on it has nothing to do.
        assert_eq!(codes(&outside, &[]), [100, 200]);
        assert_eq!(codes(&outside, &[("core", 200)]), Vec::<u64>::new());
        // Inside: the new base; staging it replaces the same-id layers.
        assert_eq!(codes(&inside, &[("core", 200)]), [300]);
        // A fresh device inside skips the old chain: the newer base is eligible
        // and staging it would replace the older one anyway.
        assert_eq!(codes(&inside, &[]), [300]);
    }

    fn codes(outcome: &Plan) -> Vec<u64> {
        match outcome {
            Plan::Apply { packs, .. } => packs.iter().map(|p| p.version_code).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_newer_eligible_base_supersedes_the_old_chain() {
        let m = manifest(vec![
            pack(PackKind::Base, 1, None),
            pack(PackKind::Patch, 2, Some(1)),
            pack(PackKind::Base, 3, None),
        ]);
        let outcome = run(&m, &ctx("2.3.0", &[]));
        assert_eq!(codes(&outcome), [3]);
        let reasons: Vec<_> = outcome
            .skipped()
            .iter()
            .map(|s| (s.version_code, s.reason.clone()))
            .collect();
        assert_eq!(
            reasons,
            [
                (1, Skipped::Superseded { by: 3 }),
                (2, Skipped::Superseded { by: 3 }),
            ]
        );
    }

    #[test]
    fn an_ineligible_newer_base_does_not_supersede() {
        let m = manifest(vec![
            pack(PackKind::Base, 1, None),
            pack(PackKind::Patch, 2, Some(1)),
            pack(PackKind::Base, 3, None),
        ]);
        let base3 = sha256_hex(b"pack-3");
        let outcome = plan(&m, &ctx("2.3.0", &[]), &|sha, _, _| *sha == base3);
        assert_eq!(codes(&outcome), [1, 2]);
        assert_eq!(outcome.skipped()[0].reason, Skipped::Blacklisted);

        // Outside its rollout bucket, the newer base does not supersede either.
        let mut m = m;
        m.packs[2].rollout = 10;
        let outside = (0..500)
            .map(|i| format!("device-{i}"))
            .find(|id| !in_rollout(id, &m.packs[2]))
            .expect("a 10% rollout should exclude someone");
        let mut c = ctx("2.3.0", &[]);
        c.install_id = outside;
        assert_eq!(codes(&run(&m, &c)), [1, 2]);
    }

    #[test]
    fn a_pack_outside_its_shell_range_is_skipped() {
        let mut p = pack(PackKind::Base, 100, None);
        p.min_shell = Some("2.0.0".parse().unwrap());
        p.max_shell = Some("2.5.0".parse().unwrap());
        let m = manifest(vec![p]);

        let outcome = run(&m, &ctx("1.9.0", &[]));
        assert!(!outcome.has_work());
        assert_eq!(
            outcome.skipped()[0].reason,
            Skipped::ShellTooOld {
                required: "2.0.0".parse().unwrap()
            }
        );

        let outcome = run(&m, &ctx("2.6.0", &[]));
        assert!(!outcome.has_work());
        assert_eq!(
            outcome.skipped()[0].reason,
            Skipped::ShellTooNew {
                supported: "2.5.0".parse().unwrap()
            }
        );

        // Both bounds are inclusive.
        assert!(run(&m, &ctx("2.0.0", &[])).has_work());
        assert!(run(&m, &ctx("2.5.0", &[])).has_work());
    }

    #[test]
    fn a_newer_base_the_shell_cannot_run_does_not_supersede() {
        let mut base3 = pack(PackKind::Base, 3, None);
        base3.max_shell = Some("2.5.0".parse().unwrap());
        let m = manifest(vec![
            pack(PackKind::Base, 1, None),
            pack(PackKind::Patch, 2, Some(1)),
            base3,
        ]);
        let outcome = run(&m, &ctx("2.6.0", &[]));
        assert_eq!(codes(&outcome), [1, 2]);
        let reasons: Vec<_> = outcome
            .skipped()
            .iter()
            .map(|s| (s.version_code, s.reason.clone()))
            .collect();
        assert_eq!(
            reasons,
            [(
                3,
                Skipped::ShellTooNew {
                    supported: "2.5.0".parse().unwrap()
                }
            )]
        );
    }

    #[test]
    #[cfg(not(app_store))]
    fn a_newer_base_does_not_supersede_a_dlc_of_another_id() {
        let mut dlc = pack(PackKind::Dlc, 1, None);
        dlc.id = PackId::parse("maps").unwrap();
        let m = manifest(vec![
            dlc,
            pack(PackKind::Base, 1, None),
            pack(PackKind::Base, 3, None),
        ]);
        let outcome = run(&m, &ctx("2.3.0", &[]));
        assert_eq!(codes(&outcome), [3, 1]);
        assert_eq!(outcome.skipped().len(), 1);
        assert_eq!(outcome.skipped()[0].id.as_str(), "core");
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
    #[cfg(not(app_store))]
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

    /// One channel serves every platform, so an App Store client has to stay
    /// updatable from a manifest that also advertises DLC. Dropping the entry
    /// keeps base and patch — security fixes included — flowing; failing the
    /// document would strand the client on whatever it already has.
    #[test]
    #[cfg(app_store)]
    fn a_dlc_entry_does_not_block_the_rest_of_the_channel() {
        let raw = serde_json::json!({
            "spec": tpk_format::channel::CHANNEL_SPEC_TAG,
            "channel": "stable",
            "published_at": "2026-09-11T15:00:00Z",
            "watermark": 202609111500u64,
            "packs": [
                {
                    "id": "core", "kind": "base", "version": "1.0.0",
                    "version_code": 100,
                    "url": "https://cdn.example.com/tpk/core/100.tpk",
                    "size": 1000, "sha256": sha256_hex(b"pack-100").to_hex(),
                },
                {
                    "id": "maps", "kind": "dlc", "version": "1.0.0",
                    "version_code": 500,
                    "url": "https://cdn.example.com/tpk/maps/500.tpk",
                    "size": 1000, "sha256": sha256_hex(b"pack-500").to_hex(),
                },
            ],
        });
        let m = ChannelManifest::parse(raw.to_string().as_bytes()).unwrap();

        let Plan::Apply { packs, .. } = run(&m, &ctx("2.3.0", &[])) else {
            panic!("expected work");
        };
        assert_eq!(packs.len(), 1, "the dlc never reaches the plan");
        assert_eq!(packs[0].id.as_str(), "core");
    }

    #[test]
    #[cfg(not(app_store))]
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
    fn a_blacklisted_base_does_not_let_an_older_one_back_in() {
        // The device committed v2 and then condemned it, so nothing is
        // installed for `core`. Without the floor the id would look brand new
        // and any older signed pack could walk back in.
        let older = manifest(vec![pack(PackKind::Base, 1, None)]);
        let outcome = run(&older, &ctx_with_floor("2.3.0", &[], 2));
        assert!(!outcome.has_work(), "v1 is a downgrade");
        assert_eq!(outcome.skipped()[0].reason, Skipped::Downgrade { floor: 2 });

        // Forward is still open: this is how a condemned release is escaped.
        let newer = manifest(vec![pack(PackKind::Base, 3, None)]);
        assert_eq!(codes(&run(&newer, &ctx_with_floor("2.3.0", &[], 2))), [3]);
    }

    #[test]
    fn the_floor_refuses_older_but_still_allows_a_reinstall() {
        // After a `reset` the floor survives but the layers are gone. Refusing
        // the version the device was on would leave it stuck on the embedded
        // assets forever, so the floor is strict and equality is allowed.
        let same = manifest(vec![pack(PackKind::Base, 2, None)]);
        assert_eq!(codes(&run(&same, &ctx_with_floor("2.3.0", &[], 2))), [2]);

        // But with that same version actually installed, there is nothing to do
        // — and that is an up-to-date device, not the rollback defence firing.
        let outcome = run(&same, &ctx_with_floor("2.3.0", &[("core", 2)], 2));
        assert!(!outcome.has_work());
        assert_eq!(
            outcome.skipped()[0].reason,
            Skipped::NotNewer { installed: 2 }
        );
    }

    #[test]
    fn a_patch_on_a_condemned_base_is_parent_missing_not_a_downgrade() {
        // The two questions are answered from two different places: the patch
        // looks at what is really installed, the base at the floor.
        let m = manifest(vec![
            pack(PackKind::Patch, 3, Some(2)),
            pack(PackKind::Base, 1, None),
        ]);
        let outcome = run(&m, &ctx_with_floor("2.3.0", &[], 2));
        assert!(!outcome.has_work());
        let reasons: Vec<_> = outcome
            .skipped()
            .iter()
            .map(|s| (s.version_code, s.reason.clone()))
            .collect();
        assert_eq!(
            reasons,
            [
                (1, Skipped::Downgrade { floor: 2 }),
                (3, Skipped::ParentMissing { expected: 2 }),
            ]
        );
    }

    #[test]
    fn an_empty_manifest_is_up_to_date() {
        assert!(!run(&manifest(vec![]), &ctx("2.3.0", &[])).has_work());
    }
}
