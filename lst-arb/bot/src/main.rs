// Use mimalloc for better performance
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use ethers::prelude::*;
use ethers::types::{Address, U256};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tracing::{info, warn, error, debug, Level};
use tracing_subscriber::FmtSubscriber;

mod config;
mod rpc;
mod price;
mod detector;
mod simulator;
mod executor;
mod monitor;
mod watcher;
mod scout;
mod scheduler;

use config::{Config, ParsedConfig};
use rpc::RpcLoadBalancer;
use price::{MulticallQuoter, VenueAddresses};
use detector::OpportunityDetector;
use executor::Executor;
use monitor::Monitor;
use watcher::{CombinedWatcher, WatcherConfig, DetectionTrigger};

// Arbitrum block time is ~250ms, backup poll every 2 blocks
const BACKUP_POLL_INTERVAL_MS: u64 = 500;

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
    info!("    LST/LRT ARBITRAGE BOT v0.2.0");
    info!("    Arbitrum Event-Driven Mode");
    info!("═══════════════════════════════════════════");

    // Load configuration
    let config = Config::load_or_default();
    let parsed = ParsedConfig::from_config(&config)?;

    info!("Configuration loaded");
    info!("  Min spread: {}bps", parsed.min_spread_bps);
    info!("  Min profit: {} ETH", ethers::utils::format_ether(parsed.min_profit));
    info!("  Trade sizing: Convex optimization with 90% liquidity clamping");
    info!("  Mode: Event-driven with {}ms backup polling", BACKUP_POLL_INTERVAL_MS);

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

    // Build token list
    let tokens: Vec<(Address, String)> = config.strategy.enabled_tokens.iter()
        .filter_map(|name| {
            parsed.tokens.get(name).map(|addr| (*addr, name.clone()))
        })
        .collect();

    info!("Monitoring {} tokens: {:?}", tokens.len(),
        tokens.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>());

    // Quote amount for price discovery (actual trade size determined by solver)
    let quote_amount = ethers::utils::parse_ether("1.0")?;

    // Initialize event watcher for Arbitrum
    let watcher_config = WatcherConfig::arbitrum_lst_pools();
    let combined_watcher = CombinedWatcher::new(watcher_config, BACKUP_POLL_INTERVAL_MS);

    info!("═══════════════════════════════════════════");
    info!("Starting event-driven main loop");
    info!("  Watching: Uniswap V3 Swaps, Curve TokenExchange, Balancer Swaps");
    info!("  Backup poll: {}ms", BACKUP_POLL_INTERVAL_MS);
    info!("═══════════════════════════════════════════");

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

    // Start the combined watcher
    let mut trigger_rx = combined_watcher.start(client.clone()).await?;

    // Track statistics
    let mut event_triggers = 0u64;
    let mut backup_triggers = 0u64;
    let mut block_triggers = 0u64;
    let mut last_stats_log = Instant::now();

    // Event-driven main loop
    loop {
        // Wait for a detection trigger
        let trigger = match trigger_rx.recv().await {
            Some(t) => t,
            None => {
                error!("Watcher channel closed, restarting...");
                // Try to restart the watcher
                if let Some(new_client) = rpc_lb.get_client().await {
                    let watcher_config = WatcherConfig::arbitrum_lst_pools();
                    let combined_watcher = CombinedWatcher::new(watcher_config, BACKUP_POLL_INTERVAL_MS);
                    trigger_rx = combined_watcher.start(new_client).await?;
                    continue;
                }
                warn!("Could not restart watcher, using fallback polling");
                tokio::time::sleep(Duration::from_millis(BACKUP_POLL_INTERVAL_MS)).await;
                DetectionTrigger::BackupPoll
            }
        };

        let loop_start = Instant::now();

        // Track trigger type
        match &trigger {
            DetectionTrigger::SwapEvent(event) => {
                event_triggers += 1;
                debug!("Triggered by swap event: {:?}", event);
            }
            DetectionTrigger::NewBlock(num) => {
                block_triggers += 1;
                debug!("Triggered by new block: {}", num);
            }
            DetectionTrigger::BackupPoll => {
                backup_triggers += 1;
                debug!("Triggered by backup poll");
            }
        }

        // Log trigger statistics periodically
        if last_stats_log.elapsed() > Duration::from_secs(60) {
            info!(
                "Trigger stats (1min): events={}, blocks={}, backup={}",
                event_triggers, block_triggers, backup_triggers
            );
            event_triggers = 0;
            block_triggers = 0;
            backup_triggers = 0;
            last_stats_log = Instant::now();
        }

        // Get fresh client for this iteration
        let client = match rpc_lb.get_client().await {
            Some(c) => c,
            None => {
                warn!("No healthy RPC available, waiting...");
                continue;
            }
        };

        // Fetch all quotes in single multicall
        let fetch_start = Instant::now();
        let token_quotes = match quoter.fetch_all_quotes(
            client.clone(),
            &tokens,
            quote_amount,
        ).await {
            Ok(q) => q,
            Err(e) => {
                warn!("Failed to fetch quotes: {:?}", e);
                continue;
            }
        };
        let fetch_time = fetch_start.elapsed();

        // Detect opportunities with optimal trade sizing using convex optimization
        let detect_start = Instant::now();
        let opportunities = detector.detect_optimal(client.clone(), &token_quotes).await;
        let detect_time = detect_start.elapsed();

        // Log timing for successful scans
        let loop_time = loop_start.elapsed();
        if opportunities.is_empty() {
            // Log less frequently when no opportunities
            if loop_time.as_millis() > 50 {
                debug!(
                    "Scan: {:?}ms (fetch: {:?}, detect: {:?}) | No opportunities",
                    loop_time.as_millis(), fetch_time.as_millis(), detect_time.as_millis()
                );
            }
        } else {
            info!(
                "Scan: {:?}ms | Found {} opportunities",
                loop_time.as_millis(), opportunities.len()
            );
        }

        // Process opportunities
        for opp in opportunities {
            opp.log();
            monitor.record_opportunity(&opp).await;

            // Execute if profitable
            info!("🎯 Attempting execution...");

            match executor.execute(client.clone(), &opp).await {
                Ok(result) => {
                    monitor.record_execution(&result).await;
                }
                Err(e) => {
                    error!("Execution error: {:?}", e);
                }
            }
        }

        // Warn on slow loops (should be <50ms for Arbitrum)
        if loop_time > Duration::from_millis(100) {
            warn!("Slow loop: {:?}", loop_time);
        }
    }
}
