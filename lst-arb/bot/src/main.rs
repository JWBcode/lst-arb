// Use mimalloc for better performance
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use ethers::prelude::*;
use ethers::types::Address;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;

mod config;
mod rpc;
mod price;
mod detector;
mod simulator;
mod executor;
mod monitor;
mod scout;
mod scheduler;
mod watcher;

use config::{Config, ParsedConfig};
use rpc::RpcLoadBalancer;
use price::{MulticallQuoter, VenueAddresses};
use detector::OpportunityDetector;
use executor::Executor;
use monitor::Monitor;
use scheduler::{Scheduler, TieredPool, ScanTier};
use scout::Scout;

// Arbitrum chain ID
const ARBITRUM_CHAIN_ID: u64 = 42161;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Load environment
    dotenv::dotenv().ok();

    // Initialize logging
    let _subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();

    info!("═══════════════════════════════════════════");
    info!("    LST/LRT ARBITRAGE BOT v0.3.0");
    info!("    Tiered L2 Scheduling Mode");
    info!("═══════════════════════════════════════════");

    // Load configuration
    let config = Config::load_or_default();
    let parsed = ParsedConfig::from_config(&config)?;

    info!("Configuration loaded");
    info!("  Min spread: {}bps", parsed.min_spread_bps);
    info!("  Min profit: {} ETH", ethers::utils::format_ether(parsed.min_profit));
    info!("  Mode: Tiered L2 Scheduling (Stream/Patrol/Lazy)");

    // Initialize RPC load balancer
    let rpc_lb = Arc::new(RpcLoadBalancer::new(
        &config.rpc.primary,
        &[&config.rpc.backup1, &config.rpc.backup2],
        config.rpc.max_latency_ms,
    ).await?);

    info!("RPC connections established");

    // Initialize wallet with Arbitrum chain ID
    let private_key = std::env::var("PRIVATE_KEY")
        .expect("PRIVATE_KEY environment variable required");
    let wallet: LocalWallet = private_key.parse()?;
    let wallet = wallet.with_chain_id(ARBITRUM_CHAIN_ID);

    info!("Wallet loaded: {:?}", wallet.address());

    // Initialize components
    let quoter = Arc::new(MulticallQuoter::new(VenueAddresses {
        multicall3: parsed.venues.multicall3,
        curve_steth: parsed.venues.curve_steth,
        curve_reth: parsed.venues.curve_reth,
        balancer_vault: parsed.venues.balancer_vault,
        uniswap_quoter: parsed.venues.uniswap_quoter,
        weth: parsed.weth,
    }));

    let detector = Arc::new(OpportunityDetector::new(
        parsed.min_spread_bps,
        parsed.min_profit,
    ));

    let client = rpc_lb.get_client().await
        .ok_or_else(|| eyre::eyre!("No healthy RPC available"))?;

    let executor = Arc::new(Executor::new(
        client.clone(),
        wallet,
        parsed.arb_contract,
        config.execution.use_flashbots,
        config.execution.flashbots_relay.clone(),
        config.execution.max_gas_price_gwei,
        config.execution.max_priority_fee_gwei,
    ).await?);

    let monitor = Arc::new(Monitor::new(
        config.monitoring.telegram_bot_token.clone(),
        config.monitoring.telegram_chat_id.clone(),
    ));

    monitor.send_startup_message().await;

    // Initialize Scout and discover pools
    info!("═══════════════════════════════════════════");
    info!("Discovering pools via The Graph...");
    info!("═══════════════════════════════════════════");

    let scout = Scout::new();
    let discovered_pools = match scout.discover_safe_pools(client.clone()).await {
        Ok(pools) => {
            info!("Discovered {} safe pools from The Graph", pools.len());
            pools
        }
        Err(e) => {
            warn!("Failed to discover pools from The Graph: {:?}", e);
            warn!("Falling back to configured tokens");
            Vec::new()
        }
    };

    // Build tiered pools from discovered pools + configured tokens
    let mut tiered_pools: Vec<TieredPool> = Vec::new();

    // Add discovered pools with their ranks
    for (rank, pool) in discovered_pools.iter().enumerate() {
        tiered_pools.push(TieredPool::new(
            pool.address,
            format!("{}/{}", pool.token0_symbol, pool.token1_symbol),
            pool.token0,
            (rank + 1) as u32,
        ));
    }

    // Add configured tokens as high-priority pools if not already discovered
    let configured_tokens: Vec<(Address, String)> = config.strategy.enabled_tokens.iter()
        .filter_map(|name| {
            parsed.tokens.get(name).map(|addr| (*addr, name.clone()))
        })
        .collect();

    for (i, (token_addr, token_name)) in configured_tokens.iter().enumerate() {
        // Check if already in discovered pools
        let already_exists = tiered_pools.iter()
            .any(|p| p.token_address == *token_addr);

        if !already_exists {
            // Add configured tokens as Tier 1 (rank 1-5)
            tiered_pools.push(TieredPool::new(
                *token_addr, // Use token address as pool address placeholder
                token_name.clone(),
                *token_addr,
                (i + 1) as u32, // High priority
            ));
        }
    }

    info!("Total pools for scheduling: {}", tiered_pools.len());

    // Log tier distribution
    let tier1_count = tiered_pools.iter().filter(|p| p.tier == ScanTier::Tier1Stream).count();
    let tier2_count = tiered_pools.iter().filter(|p| p.tier == ScanTier::Tier2Patrol).count();
    let tier3_count = tiered_pools.iter().filter(|p| p.tier == ScanTier::Tier3Lazy).count();
    info!("  Tier 1 (Stream): {} pools", tier1_count);
    info!("  Tier 2 (Patrol): {} pools", tier2_count);
    info!("  Tier 3 (Lazy): {} pools", tier3_count);

    // Create scheduler
    let (scheduler, mut opportunity_rx) = Scheduler::new(quoter.clone(), detector.clone());
    scheduler.add_pools(tiered_pools).await;

    // Spawn health check task
    let rpc_lb_health = rpc_lb.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_millis(5000));
        loop {
            interval.tick().await;
            rpc_lb_health.health_check().await;
        }
    });

    // Spawn stats logging task
    let monitor_stats = monitor.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(300)); // Every 5 minutes
        loop {
            interval.tick().await;
            monitor_stats.log_summary().await;
        }
    });

    // Spawn pending TX checker (faster for Arbitrum ~250ms blocks)
    let executor_pending = executor.clone();
    let monitor_pending = monitor.clone();
    let rpc_lb_pending = rpc_lb.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_millis(500)); // Every 2 Arbitrum blocks
        loop {
            interval.tick().await;
            if let Some(client) = rpc_lb_pending.get_client().await {
                let results = executor_pending.check_pending(client).await;
                for result in results {
                    monitor_pending.record_execution(&result).await;
                }
            }
        }
    });

    // Start the scheduler (spawns tier tasks internally)
    scheduler.start(client.clone()).await?;

    // Track statistics
    let mut tier_stats: std::collections::HashMap<ScanTier, u64> = std::collections::HashMap::new();
    let mut last_stats_log = Instant::now();

    info!("═══════════════════════════════════════════");
    info!("Tiered scheduler started - listening for opportunities");
    info!("═══════════════════════════════════════════");

    // Main loop: receive opportunities from scheduler and execute
    loop {
        match opportunity_rx.recv().await {
            Some((tier, opportunities)) => {
                let tier_count = tier_stats.entry(tier).or_insert(0);
                *tier_count += 1;

                // Log tier statistics periodically
                if last_stats_log.elapsed() > Duration::from_secs(60) {
                    info!(
                        "Tier stats (1min): Stream={}, Patrol={}, Lazy={}",
                        tier_stats.get(&ScanTier::Tier1Stream).unwrap_or(&0),
                        tier_stats.get(&ScanTier::Tier2Patrol).unwrap_or(&0),
                        tier_stats.get(&ScanTier::Tier3Lazy).unwrap_or(&0),
                    );
                    tier_stats.clear();
                    last_stats_log = Instant::now();
                }

                // Get fresh client
                let client = match rpc_lb.get_client().await {
                    Some(c) => c,
                    None => {
                        warn!("No healthy RPC available for execution");
                        continue;
                    }
                };

                info!(
                    "Received {} opportunities from {} tier",
                    opportunities.len(),
                    tier
                );

                // Process opportunities
                for opp in opportunities {
                    opp.log();
                    monitor.record_opportunity(&opp).await;

                    // Execute if profitable
                    info!("🎯 Attempting execution from {} tier...", tier);

                    match executor.execute(client.clone(), &opp).await {
                        Ok(result) => {
                            monitor.record_execution(&result).await;
                        }
                        Err(e) => {
                            error!("Execution error: {:?}", e);
                        }
                    }
                }
            }
            None => {
                error!("Scheduler channel closed unexpectedly");
                // Try to restart
                tokio::time::sleep(Duration::from_secs(5)).await;
                if let Some(new_client) = rpc_lb.get_client().await {
                    if let Err(e) = scheduler.start(new_client).await {
                        error!("Failed to restart scheduler: {:?}", e);
                    }
                }
            }
        }
    }
}
