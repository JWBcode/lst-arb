// Use mimalloc for better performance
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use ethers::prelude::*;
use ethers::types::{Address, U256};
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

use config::{Config, ParsedConfig};
use rpc::RpcLoadBalancer;
use price::{PriceCache, MulticallQuoter, VenueAddresses};
use detector::OpportunityDetector;
use executor::Executor;
use monitor::Monitor;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Load environment
    dotenv::dotenv().ok();
    
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();
    
    info!("═══════════════════════════════════════════");
    info!("    LST/LRT ARBITRAGE BOT v0.1.0");
    info!("═══════════════════════════════════════════");
    
    // Load configuration
    let config = Config::load_or_default();
    let parsed = ParsedConfig::from_config(&config)?;
    
    info!("Configuration loaded");
    info!("  Min spread: {}bps", parsed.min_spread_bps);
    info!("  Min profit: {} ETH", ethers::utils::format_ether(parsed.min_profit));
    info!("  Max trade:  {} ETH", ethers::utils::format_ether(parsed.max_trade_size));
    
    // Initialize RPC load balancer
    let rpc_lb = Arc::new(RpcLoadBalancer::new(
        &config.rpc.primary,
        &[&config.rpc.backup1, &config.rpc.backup2],
        config.rpc.max_latency_ms,
    ).await?);
    
    info!("RPC connections established");
    
    // Initialize wallet
    let private_key = std::env::var("PRIVATE_KEY")
        .expect("PRIVATE_KEY environment variable required");
    let wallet: LocalWallet = private_key.parse()?;
    let wallet = wallet.with_chain_id(1u64); // Mainnet
    
    info!("Wallet loaded: {:?}", wallet.address());
    
    // Initialize components
    let price_cache = Arc::new(PriceCache::new());
    
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
        parsed.max_trade_size,
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
    
    // Trade amount (default 5 ETH)
    let trade_amount = ethers::utils::parse_ether("5.0")?;
    
    info!("═══════════════════════════════════════════");
    info!("Starting main loop ({}ms interval)", config.strategy.poll_interval_ms);
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
    
    // Spawn pending TX checker
    let executor_pending = executor.clone();
    let monitor_pending = monitor.clone();
    let rpc_lb_pending = rpc_lb.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(12)); // Every block
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
    
    // Main loop
    let mut interval = interval(Duration::from_millis(config.strategy.poll_interval_ms));
    
    loop {
        interval.tick().await;
        
        let loop_start = Instant::now();
        
        // Get client
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
            trade_amount,
        ).await {
            Ok(q) => q,
            Err(e) => {
                warn!("Failed to fetch quotes: {:?}", e);
                continue;
            }
        };
        let fetch_time = fetch_start.elapsed();
        
        // Update cache and detect opportunities
        let detect_start = Instant::now();
        let opportunities = detector.detect(&token_quotes, trade_amount);
        let detect_time = detect_start.elapsed();
        
        // Log timing
        if opportunities.is_empty() {
            // Only log occasionally when no opportunities
            if loop_start.elapsed().as_millis() % 10000 < config.strategy.poll_interval_ms as u128 {
                info!(
                    "Scan complete | Fetch: {:?} | Detect: {:?} | No opportunities",
                    fetch_time, detect_time
                );
            }
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
        
        // Log total loop time if slow
        let loop_time = loop_start.elapsed();
        if loop_time > Duration::from_millis(100) {
            warn!("Slow loop: {:?}", loop_time);
        }
    }
}
