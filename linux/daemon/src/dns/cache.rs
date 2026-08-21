//! A bounded, TTL-respecting response cache.
//!
//! Cached bytes are the upstream's own response, so the only thing rewritten
//! on the way out is the transaction id. Capacity is fixed and entries expire,
//! which is what keeps memory bounded under a flood of unique names (§21).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// What a cache lookup is keyed on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

struct Entry {
    response: Vec<u8>,
    expires: Instant,
}

pub struct Cache {
    entries: HashMap<Key, Entry>,
    capacity: usize,
    hits: u64,
    misses: u64,
}

impl Cache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(1024)),
            capacity: capacity.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// A cached response, with `id` written into it, if one is still valid.
    pub fn get(&mut self, key: &Key, id: u16) -> Option<Vec<u8>> {
        let entry = self.entries.get(key)?;
        if entry.expires <= Instant::now() {
            self.entries.remove(key);
            self.misses += 1;
            return None;
        }
        let mut response = entry.response.clone();
        response[0..2].copy_from_slice(&id.to_be_bytes());
        self.hits += 1;
        Some(response)
    }

    pub fn insert(&mut self, key: Key, response: Vec<u8>, ttl: Duration) {
        if ttl.is_zero() || response.len() < 12 {
            return;
        }
        if self.entries.len() >= self.capacity {
            self.evict();
        }
        self.entries.insert(
            key,
            Entry {
                response,
                expires: Instant::now() + ttl,
            },
        );
    }

    /// Drop expired entries; if none have expired, drop the soonest to expire.
    fn evict(&mut self) {
        let now = Instant::now();
        let before = self.entries.len();
        self.entries.retain(|_, e| e.expires > now);
        if self.entries.len() < before {
            return;
        }
        if let Some(key) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.expires)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&key);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn stats(&self) -> (usize, u64, u64) {
        (self.entries.len(), self.hits, self.misses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key { name: name.into(), qtype: 1, qclass: 1 }
    }

    #[test]
    fn returns_the_entry_with_the_callers_transaction_id() {
        let mut cache = Cache::new(4);
        let mut response = vec![0xaa, 0xbb];
        response.extend_from_slice(&[0u8; 10]);
        cache.insert(key("a.test"), response, Duration::from_secs(60));
        let got = cache.get(&key("a.test"), 0x1234).unwrap();
        assert_eq!(&got[0..2], &[0x12, 0x34]);
    }

    #[test]
    fn expired_entries_are_not_served() {
        let mut cache = Cache::new(4);
        cache.insert(key("a.test"), vec![0u8; 12], Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get(&key("a.test"), 1).is_none());
    }

    #[test]
    fn capacity_is_enforced() {
        let mut cache = Cache::new(8);
        for i in 0..100 {
            cache.insert(key(&format!("{i}.test")), vec![0u8; 12], Duration::from_secs(60));
        }
        assert!(cache.stats().0 <= 8, "cache grew to {}", cache.stats().0);
    }
}
