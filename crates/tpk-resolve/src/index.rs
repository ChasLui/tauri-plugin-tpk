//! The overlay index: which layer wins for each path.

use std::collections::HashMap;

use tpk_format::manifest::Op;

use crate::layer::Layer;

/// Where a path's winning entry lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    /// Index into the layer stack.
    pub layer: u16,
    /// Index into that layer's entry list.
    pub entry: u32,
    /// What the winning entry does, so the common case needs no indirection.
    pub op: Op,
}

/// A frozen view of the stack: one winning entry per visible path.
///
/// Only the topmost entry for a path is kept. Everything the resolver needs to
/// answer `get` is decided here — a tombstone shadows what is below it, a `full`
/// replaces it, and a `delta` was already materialized when its layer was
/// staged. Keeping one `Loc` per path rather than the whole stack is also what
/// holds index memory to roughly 150 bytes per entry.
#[derive(Debug, Default)]
pub struct Index {
    layers: Vec<Layer>,
    by_path: HashMap<Box<str>, Loc>,
}

impl Index {
    /// The layer stack, lowest first.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Look up the winning entry for a path.
    pub fn lookup(&self, path: &str) -> Option<Loc> {
        self.by_path.get(path).copied()
    }

    /// Every path the overlay serves, tombstones excluded.
    pub fn visible_paths(&self) -> impl Iterator<Item = &str> {
        self.by_path
            .iter()
            .filter(|(_, loc)| loc.op != Op::Delete)
            .map(|(path, _)| path.as_ref())
    }

    /// Number of indexed paths, including tombstones.
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Whether the overlay is empty.
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// Resolve a location back to its entry.
    pub fn entry(&self, loc: Loc) -> Option<&tpk_format::manifest::Entry> {
        self.layers
            .get(loc.layer as usize)?
            .manifest
            .entries
            .get(loc.entry as usize)
    }

    /// Add a layer above everything already present.
    ///
    /// Returns `false` when the stack is already at its ceiling of
    /// `u16::MAX` layers.
    pub(crate) fn push(&mut self, layer: Layer) -> bool {
        let Ok(layer_idx) = u16::try_from(self.layers.len()) else {
            return false;
        };
        for (entry_idx, entry) in layer.manifest.entries.iter().enumerate() {
            let Ok(entry_idx) = u32::try_from(entry_idx) else {
                return false;
            };
            // Later layers overwrite earlier ones, which is exactly the overlay
            // rule: whatever is highest wins, whether it adds, replaces or hides.
            self.by_path.insert(
                entry.path.as_str().into(),
                Loc {
                    layer: layer_idx,
                    entry: entry_idx,
                    op: entry.op,
                },
            );
        }
        self.layers.push(layer);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::manifest::{Entry, PackId, PackKind, PackManifest, PackPolicies};
    use tpk_format::path::PackPath;
    use tpk_format::sign::sha256_hex;

    fn entry(path: &str, op: Op) -> Entry {
        let sha = sha256_hex(path.as_bytes());
        Entry {
            path: PackPath::parse(path).unwrap(),
            op,
            size: (op != Op::Delete).then_some(1),
            sha256: (op != Op::Delete).then_some(sha),
            blob: (op != Op::Delete).then(|| format!("blobs/{sha}")),
            blob_sha256: (op != Op::Delete).then_some(sha),
            blob_size: (op != Op::Delete).then_some(1),
            encoding: (op != Op::Delete).then_some(tpk_format::manifest::Encoding::Identity),
            delta_base_sha256: (op == Op::Delta).then_some(sha),
        }
    }

    fn layer(id: &str, kind: PackKind, version_code: u64, entries: Vec<Entry>) -> Layer {
        Layer {
            path: format!("/tmp/{id}-{version_code}.tpk").into(),
            file_sha256: sha256_hex(id.as_bytes()),
            manifest: PackManifest {
                spec: tpk_format::manifest::SPEC_TAG.to_string(),
                kind,
                id: PackId::parse(id).unwrap(),
                version: "1.0.0".parse().unwrap(),
                version_code,
                min_shell: None,
                max_shell: None,
                parent: None,
                created_at: "2026-09-11T15:00:00Z".to_string(),
                channel: None,
                policies: PackPolicies::default(),
                entries,
            },
        }
    }

    #[test]
    fn a_higher_layer_wins() {
        let mut index = Index::default();
        index.push(layer(
            "core",
            PackKind::Base,
            1,
            vec![entry("/a.js", Op::Full)],
        ));
        index.push(layer(
            "core",
            PackKind::Patch,
            2,
            vec![entry("/a.js", Op::Full)],
        ));

        let loc = index.lookup("/a.js").unwrap();
        assert_eq!(loc.layer, 1, "the patch, not the base");
    }

    #[test]
    fn a_tombstone_hides_what_is_below() {
        let mut index = Index::default();
        index.push(layer(
            "core",
            PackKind::Base,
            1,
            vec![entry("/gone.css", Op::Full)],
        ));
        index.push(layer(
            "core",
            PackKind::Patch,
            2,
            vec![entry("/gone.css", Op::Delete)],
        ));

        assert_eq!(index.lookup("/gone.css").unwrap().op, Op::Delete);
        assert!(!index.visible_paths().any(|p| p == "/gone.css"));
    }

    #[test]
    fn a_higher_full_overrides_a_lower_delete() {
        // Order matters in both directions: a later layer may resurrect a path
        // an earlier one tombstoned.
        let mut index = Index::default();
        index.push(layer(
            "core",
            PackKind::Base,
            1,
            vec![entry("/x.js", Op::Delete)],
        ));
        index.push(layer(
            "core",
            PackKind::Patch,
            2,
            vec![entry("/x.js", Op::Full)],
        ));

        assert_eq!(index.lookup("/x.js").unwrap().op, Op::Full);
        assert!(index.visible_paths().any(|p| p == "/x.js"));
    }

    #[test]
    fn paths_from_different_layers_all_stay_visible() {
        let mut index = Index::default();
        index.push(layer(
            "core",
            PackKind::Base,
            1,
            vec![entry("/a.js", Op::Full), entry("/b.js", Op::Full)],
        ));
        index.push(layer(
            "maps",
            PackKind::Dlc,
            1,
            vec![entry("/c.js", Op::Full)],
        ));

        let mut visible: Vec<_> = index.visible_paths().collect();
        visible.sort_unstable();
        assert_eq!(visible, ["/a.js", "/b.js", "/c.js"]);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn entries_resolve_back_from_their_location() {
        let mut index = Index::default();
        index.push(layer(
            "core",
            PackKind::Base,
            1,
            vec![entry("/a.js", Op::Full), entry("/b.js", Op::Delete)],
        ));

        let loc = index.lookup("/b.js").unwrap();
        let resolved = index.entry(loc).unwrap();
        assert_eq!(resolved.path.as_str(), "/b.js");
        assert_eq!(resolved.op, Op::Delete);
    }

    #[test]
    fn an_unknown_path_has_no_location() {
        let index = Index::default();
        assert!(index.lookup("/nope.js").is_none());
        assert!(index.is_empty());
    }
}
