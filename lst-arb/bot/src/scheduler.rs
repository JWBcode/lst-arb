//! Pool Scheduler and Garbage Collection
//!
//! Implements Active Defense mechanisms for resource management:
//! - Tiered pool monitoring (Stream, Poll, Lazy)
//! - Automatic downgrade of unproductive pools
//! - WebSocket connection management

use ethers::types::Address;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Pool monitoring tiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolTier {
    /// Tier 1: Active WebSocket stream subscription
    /// Used for high-activity pools with recent arbitrage opportunities
    Stream,
    /// Tier 2: Regular polling (every ~1 second)
    /// Used for moderate-activity pools
    Poll,
    /// Tier 3: Lazy polling (every ~30 seconds)
    /// Used for low-activity pools or those without recent opportunities
    Lazy,
}

impl PoolTier {
    /// Get the polling interval for this tier
    pub fn poll_interval(&self) -> Duration {
        match self {
            PoolTier::Stream => Duration::from_millis(0), // Event-driven, no polling
            PoolTier::Poll => Duration::from_secs(1),
            PoolTier::Lazy => Duration::from_secs(30),
        }
    }
}

/// Target pool with monitoring metadata
#[derive(Debug, Clone)]
pub struct TargetPool {
    /// Pool contract address
    pub address: Address,
    /// Current monitoring tier
    pub tier: PoolTier,
    /// Timestamp of last profitable arbitrage opportunity found
    pub last_opportunity_ts: Option<Instant>,
    /// Timestamp when pool was added to monitoring
    pub added_ts: Instant,
    /// Number of opportunities found since added
    pub opportunity_count: u64,
    /// Whether the WebSocket subscription is active (for Tier 1)
    pub ws_active: bool,
    /// Pool identifier/name for logging
    pub name: String,
}

impl TargetPool {
    pub fn new(address: Address, name: String, tier: PoolTier) -> Self {
        Self {
            address,
            tier,
            last_opportunity_ts: None,
            added_ts: Instant::now(),
            opportunity_count: 0,
            ws_active: tier == PoolTier::Stream,
            name,
        }
    }

    /// Record that an arbitrage opportunity was found
    pub fn record_opportunity(&mut self) {
        self.last_opportunity_ts = Some(Instant::now());
        self.opportunity_count += 1;
    }

    /// Check if pool should be downgraded based on inactivity
    /// Returns true if no profitable arbs found in the specified duration
    pub fn should_downgrade(&self, inactivity_threshold: Duration) -> bool {
        match self.last_opportunity_ts {
            Some(ts) => ts.elapsed() > inactivity_threshold,
            // If never had an opportunity, check against added time
            None => self.added_ts.elapsed() > inactivity_threshold,
        }
    }
}

/// Pool Scheduler for managing monitored pools
pub struct PoolScheduler {
    /// All tracked pools by address
    pools: Arc<RwLock<HashMap<Address, TargetPool>>>,
    /// Inactivity threshold for downgrade (default: 60 minutes)
    inactivity_threshold: Duration,
    /// Cleanup cycle interval (default: 5 minutes)
    cleanup_interval: Duration,
}

impl PoolScheduler {
    pub fn new() -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            inactivity_threshold: Duration::from_secs(60 * 60), // 60 minutes
            cleanup_interval: Duration::from_secs(5 * 60),       // 5 minutes
        }
    }

    pub fn with_thresholds(inactivity_minutes: u64, cleanup_minutes: u64) -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            inactivity_threshold: Duration::from_secs(inactivity_minutes * 60),
            cleanup_interval: Duration::from_secs(cleanup_minutes * 60),
        }
    }

    /// Add a pool to monitoring
    pub async fn add_pool(&self, address: Address, name: String, tier: PoolTier) {
        let mut pools = self.pools.write().await;
        pools.insert(address, TargetPool::new(address, name.clone(), tier));
        info!("Added pool {} ({:?}) to {:?} tier", name, address, tier);
    }

    /// Record an opportunity for a pool (resets inactivity timer)
    pub async fn record_opportunity(&self, address: Address) {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(&address) {
            pool.record_opportunity();
            debug!(
                "Recorded opportunity for pool {} (total: {})",
                pool.name, pool.opportunity_count
            );
        }
    }

    /// Upgrade a pool to a higher tier
    pub async fn upgrade_pool(&self, address: Address, new_tier: PoolTier) {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(&address) {
            let old_tier = pool.tier;
            pool.tier = new_tier;
            pool.ws_active = new_tier == PoolTier::Stream;
            info!(
                "Upgraded pool {} from {:?} to {:?}",
                pool.name, old_tier, new_tier
            );
        }
    }

    /// Downgrade a pool to a lower tier
    pub async fn downgrade_pool(&self, address: Address, new_tier: PoolTier) -> bool {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(&address) {
            let old_tier = pool.tier;
            if new_tier as u8 > old_tier as u8 {
                // Close WebSocket if downgrading from Stream
                if old_tier == PoolTier::Stream && new_tier != PoolTier::Stream {
                    pool.ws_active = false;
                    info!(
                        "Closing WebSocket for pool {} (downgrading to {:?})",
                        pool.name, new_tier
                    );
                }
                pool.tier = new_tier;
                info!(
                    "Downgraded pool {} from {:?} to {:?}",
                    pool.name, old_tier, new_tier
                );
                return true;
            }
        }
        false
    }

    /// Get all pools at a specific tier
    pub async fn get_pools_by_tier(&self, tier: PoolTier) -> Vec<TargetPool> {
        let pools = self.pools.read().await;
        pools
            .values()
            .filter(|p| p.tier == tier)
            .cloned()
            .collect()
    }

    /// Get all stream (Tier 1) pool addresses
    pub async fn get_stream_addresses(&self) -> Vec<Address> {
        let pools = self.pools.read().await;
        pools
            .values()
            .filter(|p| p.tier == PoolTier::Stream)
            .map(|p| p.address)
            .collect()
    }

    /// Run cleanup cycle - downgrade inactive pools
    /// Should be called every 5 minutes
    pub async fn run_cleanup_cycle(&self) -> CleanupResult {
        let mut result = CleanupResult::default();
        let pools = self.pools.read().await;

        // Find pools that need downgrading
        let to_downgrade: Vec<Address> = pools
            .values()
            .filter(|p| p.tier == PoolTier::Stream && p.should_downgrade(self.inactivity_threshold))
            .map(|p| p.address)
            .collect();

        drop(pools); // Release read lock before taking write lock

        // Downgrade each pool
        for address in to_downgrade {
            if self.downgrade_pool(address, PoolTier::Lazy).await {
                result.pools_downgraded += 1;
                result.websockets_closed += 1;
            }
        }

        if result.pools_downgraded > 0 {
            warn!(
                "Cleanup cycle: downgraded {} pools, closed {} WebSockets",
                result.pools_downgraded, result.websockets_closed
            );
        } else {
            debug!("Cleanup cycle: no pools needed downgrading");
        }

        result
    }

    /// Start the automatic cleanup task
    /// Runs every 5 minutes (configurable)
    pub fn start_cleanup_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let interval = self.cleanup_interval;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip immediate first tick
            ticker.tick().await;

            loop {
                ticker.tick().await;
                let result = self.run_cleanup_cycle().await;
                if result.pools_downgraded > 0 {
                    info!(
                        "Scheduled cleanup: {} pools downgraded",
                        result.pools_downgraded
                    );
                }
            }
        })
    }

    /// Get scheduler statistics
    pub async fn get_stats(&self) -> SchedulerStats {
        let pools = self.pools.read().await;

        let mut stats = SchedulerStats::default();

        for pool in pools.values() {
            match pool.tier {
                PoolTier::Stream => stats.tier1_count += 1,
                PoolTier::Poll => stats.tier2_count += 1,
                PoolTier::Lazy => stats.tier3_count += 1,
            }
            if pool.ws_active {
                stats.active_websockets += 1;
            }
            stats.total_opportunities += pool.opportunity_count;
        }

        stats.total_pools = pools.len();
        stats
    }
}

impl Default for PoolScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a cleanup cycle
#[derive(Debug, Default)]
pub struct CleanupResult {
    /// Number of pools downgraded
    pub pools_downgraded: usize,
    /// Number of WebSocket connections closed
    pub websockets_closed: usize,
}

/// Scheduler statistics
#[derive(Debug, Default)]
pub struct SchedulerStats {
    /// Total pools being monitored
    pub total_pools: usize,
    /// Pools at Tier 1 (Stream)
    pub tier1_count: usize,
    /// Pools at Tier 2 (Poll)
    pub tier2_count: usize,
    /// Pools at Tier 3 (Lazy)
    pub tier3_count: usize,
    /// Active WebSocket connections
    pub active_websockets: usize,
    /// Total opportunities found across all pools
    pub total_opportunities: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_lifecycle() {
        let scheduler = PoolScheduler::new();

        let addr: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();

        // Add pool at Tier 1
        scheduler
            .add_pool(addr, "Test Pool".to_string(), PoolTier::Stream)
            .await;

        let pools = scheduler.get_pools_by_tier(PoolTier::Stream).await;
        assert_eq!(pools.len(), 1);
        assert!(pools[0].ws_active);

        // Record opportunity
        scheduler.record_opportunity(addr).await;

        // Downgrade to Tier 3
        let result = scheduler.downgrade_pool(addr, PoolTier::Lazy).await;
        assert!(result);

        let stream_pools = scheduler.get_pools_by_tier(PoolTier::Stream).await;
        assert!(stream_pools.is_empty());

        let lazy_pools = scheduler.get_pools_by_tier(PoolTier::Lazy).await;
        assert_eq!(lazy_pools.len(), 1);
        assert!(!lazy_pools[0].ws_active);
    }

    #[test]
    fn test_should_downgrade() {
        let addr: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();

        let mut pool = TargetPool::new(addr, "Test".to_string(), PoolTier::Stream);

        // Pool just created, should not downgrade for 60 min threshold
        assert!(!pool.should_downgrade(Duration::from_secs(3600)));

        // But should downgrade for 0 sec threshold
        assert!(pool.should_downgrade(Duration::from_secs(0)));

        // Record opportunity, should reset timer
        pool.record_opportunity();
        assert!(!pool.should_downgrade(Duration::from_secs(3600)));
    }

    #[tokio::test]
    async fn test_cleanup_cycle() {
        let scheduler = PoolScheduler::with_thresholds(0, 5); // 0 minute inactivity threshold for testing

        let addr: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();

        scheduler
            .add_pool(addr, "Test Pool".to_string(), PoolTier::Stream)
            .await;

        // Immediate cleanup should downgrade (0 minute threshold)
        let result = scheduler.run_cleanup_cycle().await;
        assert_eq!(result.pools_downgraded, 1);
        assert_eq!(result.websockets_closed, 1);

        // Check pool is now Lazy
        let lazy_pools = scheduler.get_pools_by_tier(PoolTier::Lazy).await;
        assert_eq!(lazy_pools.len(), 1);
    }

    #[tokio::test]
    async fn test_scheduler_stats() {
        let scheduler = PoolScheduler::new();

        let addr1: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let addr2: Address = "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        let addr3: Address = "0x3333333333333333333333333333333333333333"
            .parse()
            .unwrap();

        scheduler
            .add_pool(addr1, "Pool 1".to_string(), PoolTier::Stream)
            .await;
        scheduler
            .add_pool(addr2, "Pool 2".to_string(), PoolTier::Poll)
            .await;
        scheduler
            .add_pool(addr3, "Pool 3".to_string(), PoolTier::Lazy)
            .await;

        let stats = scheduler.get_stats().await;

        assert_eq!(stats.total_pools, 3);
        assert_eq!(stats.tier1_count, 1);
        assert_eq!(stats.tier2_count, 1);
        assert_eq!(stats.tier3_count, 1);
        assert_eq!(stats.active_websockets, 1);
    }
}
