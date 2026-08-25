//! Shared daemon state.
//!
//! The engine lives behind an `Arc` swapped under a lock, which is what makes
//! filter activation atomic (§15): a reload builds the new engine completely,
//! then replaces the pointer. In-flight queries finish against the old engine
//! and the next query sees the new one; there is never a moment where the
//! daemon is filtering with a half-built rule set.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratblocker_core::{Configuration, Engine, Statistics};

use crate::dns::cache::{Cache, Key};
use crate::dns::message::BlockResponse;
use crate::dns::upstream::{Resolver, Upstream};

/// Counters the D-Bus API reports.
#[derive(Debug, Default)]
pub struct DnsCounters {
    pub queries: AtomicU64,
    pub blocked: AtomicU64,
    pub forwarded: AtomicU64,
    pub errors: AtomicU64,
    pub refused_source: AtomicU64,
}

pub struct DaemonState {
    engine: RwLock<Arc<Engine>>,
    config: RwLock<Configuration>,
    pub statistics: Statistics,
    pub counters: DnsCounters,
    pub cache: Mutex<Cache>,
    pub resolver: Resolver,
    pub block_response: BlockResponse,
    /// Minimum TTL applied to cached responses, in seconds.
    pub cache_floor: Duration,
    /// TTL served with a synthesized block answer.
    pub block_ttl: u32,
    paused_until: Mutex<Option<Instant>>,
    /// When the daemon started, for `GetStatus`.
    pub started_at: SystemTime,
    /// Epoch seconds of the last successful filter update.
    pub last_update: AtomicU64,
}

impl DaemonState {
    pub fn new(
        engine: Engine,
        config: Configuration,
        resolver: Resolver,
        cache: Cache,
        block_response: BlockResponse,
        cache_floor: Duration,
        block_ttl: u32,
    ) -> Self {
        let statistics = Statistics::new(
            config.privacy.statistics_enabled,
            config.privacy.per_domain_statistics,
        );
        Self {
            engine: RwLock::new(Arc::new(engine)),
            config: RwLock::new(config),
            statistics,
            counters: DnsCounters::default(),
            cache: Mutex::new(cache),
            resolver,
            block_response,
            cache_floor,
            block_ttl,
            paused_until: Mutex::new(None),
            started_at: SystemTime::now(),
            last_update: AtomicU64::new(0),
        }
    }

    /// Follow the machine's resolvers if they have changed, and drop anything
    /// learned from the network they replaced. Returns true when they changed.
    ///
    /// The cache has to go with them: a lease from the previous network can be
    /// a split-horizon answer, a captive portal's answer, or simply an address
    /// that is no longer reachable through the tunnel that just came up.
    pub fn refresh_upstreams(&self) -> bool {
        if !self.resolver.refresh() {
            return false;
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        true
    }

    /// Cache an answer, unless the upstreams it came from are no longer the
    /// ones in use.
    ///
    /// A query that started before a network change can still be answered by
    /// the previous network — a Wi-Fi roam, or a tunnel whose kill switch has
    /// not closed yet — and that answer is exactly what `refresh_upstreams`
    /// just cleared the cache of. Testing under the cache lock is what makes
    /// the two exclusive: the upstreams are swapped before the cache is
    /// cleared, so an answer from a superseded set either lands before the
    /// clear or is dropped here, never refills the cache after it.
    pub fn cache_answer(
        &self,
        upstreams: &Arc<Vec<Upstream>>,
        key: Key,
        response: Vec<u8>,
        ttl: Duration,
    ) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        if !Arc::ptr_eq(upstreams, &self.resolver.active()) {
            return;
        }
        cache.insert(key, response, ttl);
    }

    /// A handle on the current engine. Cheap: one lock and an `Arc` clone.
    pub fn engine(&self) -> Arc<Engine> {
        self.engine
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Swap in a freshly built engine. This is the atomic activation point.
    pub fn replace_engine(&self, engine: Engine) {
        let mut guard = self.engine.write().unwrap_or_else(|e| e.into_inner());
        *guard = Arc::new(engine);
        drop(guard);
        // Cached answers were computed under the old rules.
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        self.last_update.store(now_epoch(), Ordering::Relaxed);
    }

    pub fn config(&self) -> Configuration {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_config(&self, config: Configuration) {
        self.statistics.set_enabled(config.privacy.statistics_enabled);
        let mut guard = self.config.write().unwrap_or_else(|e| e.into_inner());
        *guard = config;
    }

    /// True when filtering should be applied right now.
    pub fn filtering_active(&self) -> bool {
        if !self.config().enabled {
            return false;
        }
        !self.is_paused()
    }

    pub fn is_paused(&self) -> bool {
        let mut guard = self.paused_until.lock().unwrap_or_else(|e| e.into_inner());
        match *guard {
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                // The pause elapsed; clear it so status stops reporting paused.
                *guard = None;
                false
            }
            None => false,
        }
    }

    pub fn pause(&self, duration: Duration) {
        *self.paused_until.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Instant::now() + duration);
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    pub fn resume(&self) {
        *self.paused_until.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// Seconds remaining on a pause, or 0.
    pub fn pause_remaining(&self) -> u64 {
        let guard = self.paused_until.lock().unwrap_or_else(|e| e.into_inner());
        match *guard {
            Some(until) => until.saturating_duration_since(Instant::now()).as_secs(),
            None => 0,
        }
    }
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
