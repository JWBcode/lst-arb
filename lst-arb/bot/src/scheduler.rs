//! Tiered L2 Scheduling for Arbitrum Arbitrage Bot
//!
//! Implements a multi-tier scheduling system that prioritizes pools by activity:
//! - Tier 1 (Stream): Top 5 pools by volume - WebSocket subscription to Swap events
//! - Tier 2 (Patrol): Rank 6-20 - Poll every 500ms
//! - Tier 3 (Lazy): Rank 21+ - Poll every 60s with promotion on 0.5% price moves

use ethers::prelude::*;
use ethers::types::{Address, Filter, H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio::time::interval;
use tracing::{debug, info, warn, error};

use crate::rpc::WsClient;
use crate::price::{MulticallQuoter, TokenQuotes};
use crate::detector::{OpportunityDetector, Opportunity};
use crate::watcher::{SwapEvent, UNISWAP_V3_SWAP_TOPIC, UNISWAP_V2_SWAP_TOPIC,
    CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_TOKEN_EXCHANGE_UNDERLYING_TOPIC, BALANCER_SWAP_TOPIC};

/// Tier intervals in milliseconds
const TIER2_PATROL_INTERVAL_MS: u64 = 1000;  // 1 second for Tier 2
const TIER3_LAZY_INTERVAL_MS: u64 = 60_000;  // 60 seconds for Tier 3

/// Promotion threshold: 0.5% price move
const PROMOTION_THRESHOLD: f64 = 0.005;

/// How long a promoted pool stays in Tier 1 (1 hour)
const PROMOTION_DURATION_SECS: u64 = 3600;

/// Scan tier classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScanTier {
    /// Top 5 pools by volume - WebSocket subscription to Swap events
    Tier1Stream,
    /// Rank 6-20 - Poll every 500ms
    Tier2Patrol,
    /// Rank 21+ - Poll every 60s with promotion on price moves
    Tier3Lazy,
}

impl std::fmt::Display for ScanTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanTier::Tier1Stream => write!(f, "Tier1-Stream"),
            ScanTier::Tier2Patrol => write!(f, "Tier2-Patrol"),
            ScanTier::Tier3Lazy => write!(f, "Tier3-Lazy"),
        }
    }
}

/// Pool information with tier assignment
#[derive(Debug, Clone)]
pub struct TieredPool {
    /// Pool contract address
    pub address: Address,
    /// Token symbol
    pub token_name: String,
    /// Token address
    pub token_address: Address,
    /// Current tier
    pub tier: ScanTier,
    /// Volume rank (1 = highest volume)
    pub volume_rank: u32,
    /// Last known price (for promotion detection)
    pub last_price: Option<U256>,
    /// When this pool was promoted to Tier 1 (if applicable)
    pub promotion_time: Option<Instant>,
    /// Original tier before promotion
    pub original_tier: Option<ScanTier>,
}

impl TieredPool {
    pub fn new(address: Address, token_name: String, token_address: Address, volume_rank: u32) -> Self {
        let tier = Self::tier_from_rank(volume_rank);
        Self {
            address,
            token_name,
            token_address,
            tier,
            volume_rank,
            last_price: None,
            promotion_time: None,
            original_tier: None,
        }
    }

    /// Determine tier based on volume rank
    fn tier_from_rank(rank: u32) -> ScanTier {
        match rank {
            1..=5 => ScanTier::Tier1Stream,
            6..=20 => ScanTier::Tier2Patrol,
            _ => ScanTier::Tier3Lazy,
        }
    }

    /// Promote this pool to Tier 1
    pub fn promote_to_tier1(&mut self) {
        if self.tier != ScanTier::Tier1Stream {
            info!("Promoting pool {} ({}) to Tier 1", self.token_name, self.address);
            self.original_tier = Some(self.tier);
            self.tier = ScanTier::Tier1Stream;
            self.promotion_time = Some(Instant::now());
        }
    }

    /// Demote pool back to original tier if promotion expired
    pub fn check_demotion(&mut self) -> bool {
        if let Some(promotion_time) = self.promotion_time {
            if promotion_time.elapsed() > Duration::from_secs(PROMOTION_DURATION_SECS) {
                if let Some(original) = self.original_tier.take() {
                    info!("Demoting pool {} ({}) back to {}", self.token_name, self.address, original);
                    self.tier = original;
                    self.promotion_time = None;
                    return true;
                }
            }
        }
        false
    }
}

/// Scheduler detection result
#[derive(Debug, Clone)]
pub struct SchedulerResult {
    pub tier: ScanTier,
    pub pool_count: usize,
    pub opportunities: Vec<Opportunity>,
    pub scan_duration: Duration,
}

/// Main scheduler managing tiered pool scanning
pub struct Scheduler {
    /// All pools indexed by address
    pools: Arc<RwLock<HashMap<Address, TieredPool>>>,
    /// Token addresses with their pools (for quote fetching)
    tokens: Arc<RwLock<Vec<(Address, String)>>>,
    /// Price quoter
    quoter: Arc<MulticallQuoter>,
    /// Opportunity detector
    detector: Arc<OpportunityDetector>,
    /// Channel to receive detected opportunities
    opportunity_tx: mpsc::UnboundedSender<(ScanTier, Vec<Opportunity>)>,
}

impl Scheduler {
    /// Create a new scheduler
    pub fn new(
        quoter: Arc<MulticallQuoter>,
        detector: Arc<OpportunityDetector>,
    ) -> (Self, mpsc::UnboundedReceiver<(ScanTier, Vec<Opportunity>)>) {
        let (tx, rx) = mpsc::unbounded_channel();

        (Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            tokens: Arc::new(RwLock::new(Vec::new())),
            quoter,
            detector,
            opportunity_tx: tx,
        }, rx)
    }

    /// Add pools to the scheduler
    pub async fn add_pools(&self, pools: Vec<TieredPool>) {
        let mut pool_map = self.pools.write().await;
        let mut tokens = self.tokens.write().await;

        for pool in pools {
            // Add to tokens list if not already present
            if !tokens.iter().any(|(addr, _)| *addr == pool.token_address) {
                tokens.push((pool.token_address, pool.token_name.clone()));
            }
            pool_map.insert(pool.address, pool);
        }

        info!("Scheduler tracking {} pools, {} tokens", pool_map.len(), tokens.len());
    }

    /// Get pools by tier
    pub async fn get_pools_by_tier(&self, tier: ScanTier) -> Vec<TieredPool> {
        let pools = self.pools.read().await;
        pools.values()
            .filter(|p| p.tier == tier)
            .cloned()
            .collect()
    }

    /// Start all scheduler tasks
    pub async fn start(
        &self,
        client: Arc<WsClient>,
    ) -> eyre::Result<()> {
        info!("═══════════════════════════════════════════");
        info!("Starting Tiered L2 Scheduler");
        info!("  Tier 1 (Stream): WebSocket events for top 5 pools");
        info!("  Tier 2 (Patrol): {}ms polling for rank 6-20", TIER2_PATROL_INTERVAL_MS);
        info!("  Tier 3 (Lazy): {}s polling for rank 21+", TIER3_LAZY_INTERVAL_MS / 1000);
        info!("  Promotion: {}% price move -> Tier 1 for 1 hour", PROMOTION_THRESHOLD * 100.0);
        info!("═══════════════════════════════════════════");

        // Task A: Stream - WebSocket subscription for Tier 1 pools
        self.spawn_tier1_stream(client.clone()).await?;

        // Task B: Patrol - 500ms polling for Tier 2 pools
        self.spawn_tier2_patrol(client.clone());

        // Task C: Lazy - 60s polling for Tier 3 pools with promotion
        self.spawn_tier3_lazy(client.clone());

        // Task D: Demotion checker - check promotion expirations
        self.spawn_demotion_checker();

        Ok(())
    }

    /// Task A: Maintain WebSocket subscription for Tier 1 pools
    async fn spawn_tier1_stream(&self, client: Arc<WsClient>) -> eyre::Result<()> {
        let pools = self.pools.clone();
        let tokens = self.tokens.clone();
        let quoter = self.quoter.clone();
        let detector = self.detector.clone();
        let tx = self.opportunity_tx.clone();

        tokio::spawn(async move {
            loop {
                // Get current Tier 1 pools
                let tier1_pools: Vec<TieredPool> = {
                    let pool_map = pools.read().await;
                    pool_map.values()
                        .filter(|p| p.tier == ScanTier::Tier1Stream)
                        .cloned()
                        .collect()
                };

                if tier1_pools.is_empty() {
                    debug!("No Tier 1 pools, waiting...");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }

                // Build filter for Tier 1 pool addresses
                let addresses: Vec<Address> = tier1_pools.iter()
                    .map(|p| p.address)
                    .collect();

                let topics: Vec<H256> = vec![
                    UNISWAP_V3_SWAP_TOPIC.parse().unwrap(),
                    UNISWAP_V2_SWAP_TOPIC.parse().unwrap(),
                    CURVE_TOKEN_EXCHANGE_TOPIC.parse().unwrap(),
                    CURVE_TOKEN_EXCHANGE_UNDERLYING_TOPIC.parse().unwrap(),
                    BALANCER_SWAP_TOPIC.parse().unwrap(),
                ];

                let filter = Filter::new()
                    .address(addresses.clone())
                    .topic0(topics);

                info!("Tier 1 Stream: Subscribing to {} pools", addresses.len());

                // Subscribe to logs
                let mut stream = match client.subscribe_logs(&filter).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!("Failed to subscribe to Tier 1 logs: {:?}", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                while let Some(log) = stream.next().await {
                    let scan_start = Instant::now();
                    debug!("Tier 1 event from pool: {:?}", log.address);

                    // Get tokens for Tier 1 pools only
                    let tier1_tokens: Vec<(Address, String)> = {
                        let pool_map = pools.read().await;
                        let token_list = tokens.read().await;
                        tier1_pools.iter()
                            .filter_map(|p| {
                                token_list.iter()
                                    .find(|(addr, _)| *addr == p.token_address)
                                    .cloned()
                            })
                            .collect()
                    };

                    if tier1_tokens.is_empty() {
                        continue;
                    }

                    // Fetch quotes and detect opportunities
                    let quote_amount = ethers::utils::parse_ether("1.0").unwrap();
                    match quoter.fetch_all_quotes(client.clone(), &tier1_tokens, quote_amount).await {
                        Ok(token_quotes) => {
                            let opportunities = detector.detect_optimal(client.clone(), &token_quotes).await;
                            if !opportunities.is_empty() {
                                let _ = tx.send((ScanTier::Tier1Stream, opportunities));
                            }
                        }
                        Err(e) => {
                            warn!("Tier 1 quote fetch failed: {:?}", e);
                        }
                    }

                    debug!("Tier 1 scan took {:?}", scan_start.elapsed());
                }

                warn!("Tier 1 stream ended, reconnecting...");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        Ok(())
    }

    /// Task B: 500ms polling for Tier 2 pools
    fn spawn_tier2_patrol(&self, client: Arc<WsClient>) {
        let pools = self.pools.clone();
        let tokens = self.tokens.clone();
        let quoter = self.quoter.clone();
        let detector = self.detector.clone();
        let tx = self.opportunity_tx.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(TIER2_PATROL_INTERVAL_MS));

            loop {
                interval.tick().await;
                let scan_start = Instant::now();

                // Get current Tier 2 pools
                let tier2_pools: Vec<TieredPool> = {
                    let pool_map = pools.read().await;
                    pool_map.values()
                        .filter(|p| p.tier == ScanTier::Tier2Patrol)
                        .cloned()
                        .collect()
                };

                if tier2_pools.is_empty() {
                    continue;
                }

                // Get tokens for Tier 2 pools
                let tier2_tokens: Vec<(Address, String)> = {
                    let token_list = tokens.read().await;
                    tier2_pools.iter()
                        .filter_map(|p| {
                            token_list.iter()
                                .find(|(addr, _)| *addr == p.token_address)
                                .cloned()
                        })
                        .collect()
                };

                if tier2_tokens.is_empty() {
                    continue;
                }

                // Fetch quotes and detect opportunities
                let quote_amount = ethers::utils::parse_ether("1.0").unwrap();
                match quoter.fetch_all_quotes(client.clone(), &tier2_tokens, quote_amount).await {
                    Ok(token_quotes) => {
                        let opportunities = detector.detect_optimal(client.clone(), &token_quotes).await;
                        if !opportunities.is_empty() {
                            let _ = tx.send((ScanTier::Tier2Patrol, opportunities));
                        }
                    }
                    Err(e) => {
                        debug!("Tier 2 quote fetch failed: {:?}", e);
                    }
                }

                let elapsed = scan_start.elapsed();
                if elapsed > Duration::from_millis(TIER2_PATROL_INTERVAL_MS / 2) {
                    debug!("Tier 2 patrol scan took {:?} ({}ms interval)", elapsed, TIER2_PATROL_INTERVAL_MS);
                }
            }
        });
    }

    /// Task C: 60s polling for Tier 3 pools with promotion logic
    fn spawn_tier3_lazy(&self, client: Arc<WsClient>) {
        let pools = self.pools.clone();
        let tokens = self.tokens.clone();
        let quoter = self.quoter.clone();
        let detector = self.detector.clone();
        let tx = self.opportunity_tx.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(TIER3_LAZY_INTERVAL_MS));

            loop {
                interval.tick().await;
                let scan_start = Instant::now();

                // Get current Tier 3 pools
                let tier3_pools: Vec<TieredPool> = {
                    let pool_map = pools.read().await;
                    pool_map.values()
                        .filter(|p| p.tier == ScanTier::Tier3Lazy)
                        .cloned()
                        .collect()
                };

                if tier3_pools.is_empty() {
                    continue;
                }

                // Get tokens for Tier 3 pools
                let tier3_tokens: Vec<(Address, String)> = {
                    let token_list = tokens.read().await;
                    tier3_pools.iter()
                        .filter_map(|p| {
                            token_list.iter()
                                .find(|(addr, _)| *addr == p.token_address)
                                .cloned()
                        })
                        .collect()
                };

                if tier3_tokens.is_empty() {
                    continue;
                }

                // Fetch quotes
                let quote_amount = ethers::utils::parse_ether("1.0").unwrap();
                match quoter.fetch_all_quotes(client.clone(), &tier3_tokens, quote_amount).await {
                    Ok(token_quotes) => {
                        // Check for price movements and promote if needed
                        Self::check_promotions(&pools, &token_quotes).await;

                        // Detect opportunities
                        let opportunities = detector.detect_optimal(client.clone(), &token_quotes).await;
                        if !opportunities.is_empty() {
                            let _ = tx.send((ScanTier::Tier3Lazy, opportunities));
                        }
                    }
                    Err(e) => {
                        debug!("Tier 3 quote fetch failed: {:?}", e);
                    }
                }

                info!("Tier 3 lazy scan completed in {:?} ({} pools)",
                    scan_start.elapsed(), tier3_pools.len());
            }
        });
    }

    /// Check for price movements and promote pools to Tier 1
    async fn check_promotions(
        pools: &Arc<RwLock<HashMap<Address, TieredPool>>>,
        token_quotes: &[TokenQuotes],
    ) {
        let mut pool_map = pools.write().await;

        for tq in token_quotes {
            // Find pools for this token
            for pool in pool_map.values_mut() {
                if pool.token_address != tq.token {
                    continue;
                }

                // Get current price from quotes (use best buy price as reference)
                let current_price = tq.quotes.iter()
                    .filter(|(_, q)| q.buy_amount > U256::zero())
                    .map(|(_, q)| q.buy_amount)
                    .max();

                if let Some(current) = current_price {
                    if let Some(last) = pool.last_price {
                        // Calculate price change
                        let price_change = Self::calculate_price_change(last, current);

                        if price_change > PROMOTION_THRESHOLD {
                            info!(
                                "Pool {} ({}) price moved {:.2}% - PROMOTING to Tier 1",
                                pool.token_name,
                                pool.address,
                                price_change * 100.0
                            );
                            pool.promote_to_tier1();
                        }
                    }

                    // Update last price
                    pool.last_price = Some(current);
                }
            }
        }
    }

    /// Calculate percentage price change
    fn calculate_price_change(old_price: U256, new_price: U256) -> f64 {
        if old_price.is_zero() {
            return 0.0;
        }

        let old_f = old_price.as_u128() as f64;
        let new_f = new_price.as_u128() as f64;

        ((new_f - old_f) / old_f).abs()
    }

    /// Task D: Check for demotion of promoted pools
    fn spawn_demotion_checker(&self) {
        let pools = self.pools.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60)); // Check every minute

            loop {
                interval.tick().await;

                let mut pool_map = pools.write().await;
                let mut demoted_count = 0;

                for pool in pool_map.values_mut() {
                    if pool.check_demotion() {
                        demoted_count += 1;
                    }
                }

                if demoted_count > 0 {
                    info!("Demoted {} pools from Tier 1", demoted_count);
                }
            }
        });
    }

    /// Get statistics about current tier distribution
    pub async fn get_tier_stats(&self) -> HashMap<ScanTier, usize> {
        let pools = self.pools.read().await;
        let mut stats = HashMap::new();

        for pool in pools.values() {
            *stats.entry(pool.tier).or_insert(0) += 1;
        }

        stats
    }
}

/// Factory function to create a scheduler with pools from Scout
pub async fn create_scheduler_with_scout(
    quoter: Arc<MulticallQuoter>,
    detector: Arc<OpportunityDetector>,
    scout_pools: Vec<crate::scout::TargetPool>,
) -> (Scheduler, mpsc::UnboundedReceiver<(ScanTier, Vec<Opportunity>)>) {
    let (scheduler, rx) = Scheduler::new(quoter, detector);

    // Convert scout pools to tiered pools
    let tiered_pools: Vec<TieredPool> = scout_pools.into_iter()
        .enumerate()
        .map(|(i, pool)| {
            let rank = (i + 1) as u32;
            // Use token0 as the main token (adjust based on your needs)
            TieredPool::new(
                pool.address,
                pool.token0_symbol.clone(),
                pool.token0,
                rank,
            )
        })
        .collect();

    scheduler.add_pools(tiered_pools).await;

    (scheduler, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Tier Classification Tests
    // ========================================================================

    #[test]
    fn test_tier_from_rank() {
        assert_eq!(TieredPool::tier_from_rank(1), ScanTier::Tier1Stream);
        assert_eq!(TieredPool::tier_from_rank(5), ScanTier::Tier1Stream);
        assert_eq!(TieredPool::tier_from_rank(6), ScanTier::Tier2Patrol);
        assert_eq!(TieredPool::tier_from_rank(20), ScanTier::Tier2Patrol);
        assert_eq!(TieredPool::tier_from_rank(21), ScanTier::Tier3Lazy);
        assert_eq!(TieredPool::tier_from_rank(100), ScanTier::Tier3Lazy);
    }

    #[test]
    fn test_tier_boundaries() {
        // Test exact boundary conditions
        assert_eq!(TieredPool::tier_from_rank(5), ScanTier::Tier1Stream);
        assert_eq!(TieredPool::tier_from_rank(6), ScanTier::Tier2Patrol);
        assert_eq!(TieredPool::tier_from_rank(20), ScanTier::Tier2Patrol);
        assert_eq!(TieredPool::tier_from_rank(21), ScanTier::Tier3Lazy);
    }

    #[test]
    fn test_scan_tier_display() {
        assert_eq!(format!("{}", ScanTier::Tier1Stream), "Tier1-Stream");
        assert_eq!(format!("{}", ScanTier::Tier2Patrol), "Tier2-Patrol");
        assert_eq!(format!("{}", ScanTier::Tier3Lazy), "Tier3-Lazy");
    }

    // ========================================================================
    // Price Change Calculation Tests
    // ========================================================================

    #[test]
    fn test_price_change_calculation() {
        let old = U256::from(1000u64);
        let new = U256::from(1005u64);
        let change = Scheduler::calculate_price_change(old, new);
        assert!((change - 0.005).abs() < 0.0001);
    }

    #[test]
    fn test_price_change_negative() {
        let old = U256::from(1000u64);
        let new = U256::from(995u64); // 0.5% decrease
        let change = Scheduler::calculate_price_change(old, new);
        assert!((change - 0.005).abs() < 0.0001); // Should be absolute value
    }

    #[test]
    fn test_price_change_zero_old_price() {
        let old = U256::zero();
        let new = U256::from(1000u64);
        let change = Scheduler::calculate_price_change(old, new);
        assert_eq!(change, 0.0); // Avoid division by zero
    }

    #[test]
    fn test_price_change_no_change() {
        let old = U256::from(1000u64);
        let new = U256::from(1000u64);
        let change = Scheduler::calculate_price_change(old, new);
        assert_eq!(change, 0.0);
    }

    #[test]
    fn test_price_change_large_move() {
        let old = U256::from(1000u64);
        let new = U256::from(1100u64); // 10% increase
        let change = Scheduler::calculate_price_change(old, new);
        assert!((change - 0.1).abs() < 0.0001);
    }

    // ========================================================================
    // Promotion Threshold Tests
    // ========================================================================

    #[test]
    fn test_promotion_threshold() {
        let old = U256::from(1000u64);

        // 0.4% move - should NOT promote
        let new_small = U256::from(1004u64);
        let change_small = Scheduler::calculate_price_change(old, new_small);
        assert!(change_small < PROMOTION_THRESHOLD);

        // 0.6% move - SHOULD promote
        let new_large = U256::from(1006u64);
        let change_large = Scheduler::calculate_price_change(old, new_large);
        assert!(change_large > PROMOTION_THRESHOLD);
    }

    #[test]
    fn test_promotion_threshold_exact() {
        // Exactly 0.5% should trigger promotion (>= threshold)
        let old = U256::from(10000u64);
        let new = U256::from(10050u64);
        let change = Scheduler::calculate_price_change(old, new);
        assert!((change - 0.005).abs() < 0.0001);
        assert!(change >= PROMOTION_THRESHOLD);
    }

    // ========================================================================
    // TieredPool Tests
    // ========================================================================

    #[test]
    fn test_tiered_pool_creation() {
        let pool = TieredPool::new(
            Address::zero(),
            "TEST".to_string(),
            Address::zero(),
            1,
        );
        assert_eq!(pool.tier, ScanTier::Tier1Stream);
        assert_eq!(pool.volume_rank, 1);
        assert!(pool.last_price.is_none());
        assert!(pool.promotion_time.is_none());
        assert!(pool.original_tier.is_none());
    }

    #[test]
    fn test_tiered_pool_promotion() {
        let mut pool = TieredPool::new(
            Address::zero(),
            "TEST".to_string(),
            Address::zero(),
            25, // Tier 3
        );
        assert_eq!(pool.tier, ScanTier::Tier3Lazy);

        pool.promote_to_tier1();

        assert_eq!(pool.tier, ScanTier::Tier1Stream);
        assert_eq!(pool.original_tier, Some(ScanTier::Tier3Lazy));
        assert!(pool.promotion_time.is_some());
    }

    #[test]
    fn test_tiered_pool_promotion_idempotent() {
        let mut pool = TieredPool::new(
            Address::zero(),
            "TEST".to_string(),
            Address::zero(),
            1, // Already Tier 1
        );
        assert_eq!(pool.tier, ScanTier::Tier1Stream);

        pool.promote_to_tier1();

        // Should remain unchanged
        assert_eq!(pool.tier, ScanTier::Tier1Stream);
        assert!(pool.original_tier.is_none()); // Never set
        assert!(pool.promotion_time.is_none()); // Never set
    }

    #[test]
    fn test_tiered_pool_demotion_not_ready() {
        let mut pool = TieredPool::new(
            Address::zero(),
            "TEST".to_string(),
            Address::zero(),
            25,
        );
        pool.promote_to_tier1();

        // Immediately check demotion - should not demote (1 hour hasn't passed)
        let demoted = pool.check_demotion();
        assert!(!demoted);
        assert_eq!(pool.tier, ScanTier::Tier1Stream);
    }

    // ========================================================================
    // Interval Configuration Tests
    // ========================================================================

    #[test]
    fn test_tier2_interval() {
        assert_eq!(TIER2_PATROL_INTERVAL_MS, 1000); // 1 second
    }

    #[test]
    fn test_tier3_interval() {
        assert_eq!(TIER3_LAZY_INTERVAL_MS, 60_000); // 60 seconds
    }

    #[test]
    fn test_promotion_duration() {
        assert_eq!(PROMOTION_DURATION_SECS, 3600); // 1 hour
    }

    #[test]
    fn test_promotion_threshold_value() {
        assert_eq!(PROMOTION_THRESHOLD, 0.005); // 0.5%
    }

    // ========================================================================
    // ScanTier Equality Tests
    // ========================================================================

    #[test]
    fn test_scan_tier_equality() {
        assert_eq!(ScanTier::Tier1Stream, ScanTier::Tier1Stream);
        assert_ne!(ScanTier::Tier1Stream, ScanTier::Tier2Patrol);
        assert_ne!(ScanTier::Tier2Patrol, ScanTier::Tier3Lazy);
    }

    #[test]
    fn test_scan_tier_clone() {
        let tier = ScanTier::Tier1Stream;
        let cloned = tier.clone();
        assert_eq!(tier, cloned);
    }

    #[test]
    fn test_scan_tier_copy() {
        let tier = ScanTier::Tier1Stream;
        let copied = tier; // Copy
        assert_eq!(tier, copied);
    }

    #[test]
    fn test_scan_tier_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ScanTier::Tier1Stream);
        set.insert(ScanTier::Tier2Patrol);
        set.insert(ScanTier::Tier3Lazy);
        assert_eq!(set.len(), 3);
    }
}
