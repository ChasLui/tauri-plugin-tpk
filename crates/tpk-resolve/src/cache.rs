//! A byte-budgeted LRU for decoded assets.
//!
//! Budgeted by bytes rather than entries: a 5 MB asset and a 500 byte one cost
//! very different amounts of memory, and an entry-counted cache sized for the
//! latter will happily hold a hundred of the former.
//!
//! Objects above a fraction of the budget are refused outright — admitting one
//! would evict everything else to hold a single item that is unlikely to be
//! asked for twice.

use std::collections::HashMap;
use std::sync::Arc;

/// Largest share of the budget any single object may occupy.
const MAX_SINGLE_OBJECT_FRACTION: u64 = 4;

/// A simple byte-budgeted LRU.
#[derive(Debug)]
pub struct ByteLru {
    budget: u64,
    used: u64,
    /// Monotonic counter standing in for a clock.
    tick: u64,
    entries: HashMap<Box<str>, CacheEntry>,
}

#[derive(Debug)]
struct CacheEntry {
    bytes: Arc<[u8]>,
    last_used: u64,
}

impl ByteLru {
    /// Create a cache with the given byte budget. A budget of zero disables it.
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget: budget_bytes,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
        }
    }

    /// Whether caching is switched off.
    pub fn is_disabled(&self) -> bool {
        self.budget == 0
    }

    /// Bytes currently held.
    pub fn used_bytes(&self) -> u64 {
        self.used
    }

    /// Number of objects held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a key, marking it as recently used.
    pub fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = tick;
        Some(Arc::clone(&entry.bytes))
    }

    /// Insert a value, evicting least-recently-used entries to make room.
    ///
    /// Returns the stored handle so the caller can hand it out without a second
    /// lookup. Oversized values are returned without being stored.
    pub fn insert(&mut self, key: &str, bytes: Arc<[u8]>) -> Arc<[u8]> {
        let len = bytes.len() as u64;
        if self.budget == 0 || len > self.budget / MAX_SINGLE_OBJECT_FRACTION {
            return bytes;
        }

        if let Some(old) = self.entries.remove(key) {
            self.used -= old.bytes.len() as u64;
        }
        while self.used + len > self.budget {
            if !self.evict_one() {
                return bytes;
            }
        }

        self.tick += 1;
        self.used += len;
        self.entries.insert(
            key.into(),
            CacheEntry {
                bytes: Arc::clone(&bytes),
                last_used: self.tick,
            },
        );
        bytes
    }

    fn evict_one(&mut self) -> bool {
        let Some(victim) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| k.clone())
        else {
            return false;
        };
        if let Some(entry) = self.entries.remove(&victim) {
            self.used -= entry.bytes.len() as u64;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(n: usize) -> Arc<[u8]> {
        vec![0u8; n].into()
    }

    #[test]
    fn stores_and_returns_values() {
        let mut lru = ByteLru::new(1024);
        lru.insert("/a.js", blob(100));
        assert_eq!(lru.get("/a.js").unwrap().len(), 100);
        assert_eq!(lru.used_bytes(), 100);
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn misses_report_none() {
        let mut lru = ByteLru::new(1024);
        assert!(lru.get("/missing.js").is_none());
    }

    #[test]
    fn evicts_least_recently_used_first() {
        // Budget 400 with 100-byte objects: each is exactly at the single-object
        // ceiling (budget/4), so all of them are admissible and four fill it.
        let mut lru = ByteLru::new(400);
        for key in ["/a", "/b", "/c", "/d"] {
            lru.insert(key, blob(100));
        }
        assert_eq!(lru.used_bytes(), 400);

        // Touch /a so /b becomes the coldest.
        assert!(lru.get("/a").is_some());
        lru.insert("/e", blob(100));

        assert!(lru.get("/a").is_some(), "recently used must survive");
        assert!(lru.get("/b").is_none(), "coldest must be evicted");
        assert!(lru.get("/e").is_some());
        assert!(lru.used_bytes() <= 400);
    }

    #[test]
    fn refuses_objects_above_a_quarter_of_the_budget() {
        let mut lru = ByteLru::new(1000);
        // 300 > 1000/4, so it is handed back rather than stored.
        let returned = lru.insert("/big", blob(300));
        assert_eq!(returned.len(), 300, "the value is still usable");
        assert!(lru.get("/big").is_none(), "but it was not cached");
        assert_eq!(lru.used_bytes(), 0);

        // Just under the limit is accepted.
        lru.insert("/ok", blob(250));
        assert!(lru.get("/ok").is_some());
    }

    #[test]
    fn a_zero_budget_disables_the_cache() {
        let mut lru = ByteLru::new(0);
        assert!(lru.is_disabled());
        let returned = lru.insert("/a", blob(10));
        assert_eq!(returned.len(), 10);
        assert!(lru.get("/a").is_none());
        assert!(lru.is_empty());
    }

    #[test]
    fn reinserting_a_key_does_not_double_count() {
        let mut lru = ByteLru::new(1000);
        lru.insert("/a", blob(100));
        lru.insert("/a", blob(200));
        assert_eq!(lru.used_bytes(), 200);
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.get("/a").unwrap().len(), 200);
    }

    #[test]
    fn stays_within_budget_under_churn() {
        let mut lru = ByteLru::new(1000);
        for i in 0..100 {
            lru.insert(&format!("/f{i}"), blob(150));
            assert!(
                lru.used_bytes() <= 1000,
                "budget exceeded at iteration {i}: {}",
                lru.used_bytes()
            );
        }
    }
}
