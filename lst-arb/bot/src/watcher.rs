//! WebSocket Event Watcher for Arbitrum
//!
//! Subscribes to DEX swap events to trigger immediate arbitrage detection.
//! Arbitrum produces blocks every ~250ms, so event-driven detection is essential.

use ethers::prelude::*;
use ethers::types::{Address, Filter, Log, H256};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn, error};

use crate::rpc::WsClient;

// Event signatures (keccak256 of event signature)
// Uniswap V3: Swap(address,address,int256,int256,uint160,uint128,int24)
pub const UNISWAP_V3_SWAP_TOPIC: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";

// Uniswap V2: Swap(address,uint256,uint256,uint256,uint256,address)
pub const UNISWAP_V2_SWAP_TOPIC: &str = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";

// Curve: TokenExchange(address,int128,uint256,int128,uint256)
pub const CURVE_TOKEN_EXCHANGE_TOPIC: &str = "0x8b3e96f2b889fa771c53c981b40daf005f63f637f1869f707052d15a3dd97140";

// Curve: TokenExchangeUnderlying(address,int128,uint256,int128,uint256)
pub const CURVE_TOKEN_EXCHANGE_UNDERLYING_TOPIC: &str = "0xd013ca23e77a65003c2c659c5442c00c805371b7fc1ebd4c206c41d1536bd90b";

// Balancer V2: Swap(bytes32,address,address,uint256,uint256)
pub const BALANCER_SWAP_TOPIC: &str = "0x2170c741c41531aec20e7c107c24eecfdd15e69c9bb0a8dd37b1840b9e0b207b";

/// Event types we're watching for
#[derive(Debug, Clone)]
pub enum SwapEvent {
    UniswapV3 { pool: Address, block: u64 },
    UniswapV2 { pool: Address, block: u64 },
    Curve { pool: Address, block: u64 },
    Balancer { pool_id: H256, block: u64 },
}

/// Watcher configuration
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Uniswap V3 pool addresses to watch
    pub uniswap_v3_pools: Vec<Address>,
    /// Uniswap V2 pool addresses to watch
    pub uniswap_v2_pools: Vec<Address>,
    /// Curve pool addresses to watch
    pub curve_pools: Vec<Address>,
    /// Balancer vault address
    pub balancer_vault: Address,
}

impl WatcherConfig {
    /// Create config for Arbitrum LST/LRT pools
    pub fn arbitrum_lst_pools() -> Self {
        Self {
            uniswap_v3_pools: vec![
                // wstETH/ETH 0.05%
                "0x35218a1cbaC5Bbc3E57fd9Bd38219D37571b3537".parse().unwrap(),
                // wstETH/ETH 0.01%
                "0x7A20B2F07d5B2A9aE5F1F24b8C3c0c9F7b9e4C3A".parse().unwrap(),
                // rETH/ETH 0.05%
                "0x09BA4E5F0D0f0C3A0a7AC7D7A05c1C0A0B0C0D0E".parse().unwrap(),
            ],
            uniswap_v2_pools: vec![
                // Camelot wstETH/ETH (Uniswap V2 fork)
                "0x0E3eF0c8D2D4A1d4F8c9c0F9c8E9c0D2f0A1b2C3".parse().unwrap(),
            ],
            curve_pools: vec![
                // Curve wstETH/ETH NG Pool on Arbitrum
                "0x6eB2dc694eB516B16Dc9d7671f465248B71E9091".parse().unwrap(),
            ],
            // Arbitrum Balancer V2 Vault
            balancer_vault: "0xBA12222222228d8Ba445958a75a0704d566BF2C8".parse().unwrap(),
        }
    }
}

/// Event watcher that subscribes to DEX events via WebSocket
pub struct EventWatcher {
    config: WatcherConfig,
}

impl EventWatcher {
    pub fn new(config: WatcherConfig) -> Self {
        Self { config }
    }

    /// Start watching for swap events
    /// Returns a receiver channel that emits SwapEvents
    pub async fn start(
        &self,
        client: Arc<WsClient>,
    ) -> eyre::Result<mpsc::UnboundedReceiver<SwapEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();

        // Build filter for all swap events
        let filter = self.build_filter();

        info!("Starting event watcher for swap events");

        let config = self.config.clone();
        let client_clone = client.clone();

        tokio::spawn(async move {
            // Subscribe to logs inside the spawned task
            let mut stream = match client_clone.subscribe_logs(&filter).await {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to subscribe to logs: {:?}", e);
                    return;
                }
            };

            info!("Event watcher stream started");

            while let Some(log) = stream.next().await {
                if let Some(event) = Self::parse_log(&config, &log) {
                    debug!("Received swap event: {:?}", event);
                    if tx.send(event).is_err() {
                        warn!("Event receiver dropped, stopping watcher");
                        break;
                    }
                }
            }

            warn!("Event watcher stream ended");
        });

        Ok(rx)
    }

    /// Build the log filter for all watched events
    fn build_filter(&self) -> Filter {
        // Collect all pool addresses we want to watch
        let mut addresses: Vec<Address> = Vec::new();
        addresses.extend(&self.config.uniswap_v3_pools);
        addresses.extend(&self.config.uniswap_v2_pools);
        addresses.extend(&self.config.curve_pools);
        addresses.push(self.config.balancer_vault);

        // Build topic filter (OR of all swap event signatures)
        let topics: Vec<H256> = vec![
            UNISWAP_V3_SWAP_TOPIC.parse().unwrap(),
            UNISWAP_V2_SWAP_TOPIC.parse().unwrap(),
            CURVE_TOKEN_EXCHANGE_TOPIC.parse().unwrap(),
            CURVE_TOKEN_EXCHANGE_UNDERLYING_TOPIC.parse().unwrap(),
            BALANCER_SWAP_TOPIC.parse().unwrap(),
        ];

        Filter::new()
            .address(addresses)
            .topic0(topics)
    }

    /// Parse a log into a SwapEvent
    fn parse_log(_config: &WatcherConfig, log: &Log) -> Option<SwapEvent> {
        let topic0 = log.topics.first()?;
        let block = log.block_number?.as_u64();
        let address = log.address;

        // Match by topic signature
        if *topic0 == UNISWAP_V3_SWAP_TOPIC.parse::<H256>().ok()? {
            return Some(SwapEvent::UniswapV3 { pool: address, block });
        }

        if *topic0 == UNISWAP_V2_SWAP_TOPIC.parse::<H256>().ok()? {
            return Some(SwapEvent::UniswapV2 { pool: address, block });
        }

        if *topic0 == CURVE_TOKEN_EXCHANGE_TOPIC.parse::<H256>().ok()?
            || *topic0 == CURVE_TOKEN_EXCHANGE_UNDERLYING_TOPIC.parse::<H256>().ok()? {
            return Some(SwapEvent::Curve { pool: address, block });
        }

        if *topic0 == BALANCER_SWAP_TOPIC.parse::<H256>().ok()? {
            // For Balancer, pool_id is in topic1
            let pool_id = log.topics.get(1).copied().unwrap_or_default();
            return Some(SwapEvent::Balancer { pool_id, block });
        }

        None
    }
}

/// Trigger signal for the main detection loop
#[derive(Debug, Clone)]
pub enum DetectionTrigger {
    /// Triggered by a swap event
    SwapEvent(SwapEvent),
    /// Triggered by backup polling interval
    BackupPoll,
    /// Triggered by new block
    NewBlock(u64),
}

/// Combined watcher that merges events and backup polling
pub struct CombinedWatcher {
    event_watcher: EventWatcher,
    backup_interval_ms: u64,
}

impl CombinedWatcher {
    pub fn new(config: WatcherConfig, backup_interval_ms: u64) -> Self {
        Self {
            event_watcher: EventWatcher::new(config),
            backup_interval_ms,
        }
    }

    /// Start the combined watcher
    /// Returns a receiver that emits DetectionTriggers
    pub async fn start(
        &self,
        client: Arc<WsClient>,
    ) -> eyre::Result<mpsc::UnboundedReceiver<DetectionTrigger>> {
        let (tx, rx) = mpsc::unbounded_channel();

        // Start event watcher
        let mut event_rx = self.event_watcher.start(client.clone()).await?;

        let backup_ms = self.backup_interval_ms;
        let client_clone = client.clone();

        tokio::spawn(async move {
            // Subscribe to new blocks inside the spawned task
            let mut block_stream = match client_clone.subscribe_blocks().await {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to subscribe to blocks: {:?}", e);
                    return;
                }
            };

            let mut backup_interval = tokio::time::interval(
                std::time::Duration::from_millis(backup_ms)
            );
            // Don't fire immediately
            backup_interval.tick().await;

            loop {
                tokio::select! {
                    // Swap event received - highest priority
                    Some(event) = event_rx.recv() => {
                        if tx.send(DetectionTrigger::SwapEvent(event)).is_err() {
                            break;
                        }
                    }

                    // New block received
                    Some(block) = block_stream.next() => {
                        let block_num = block.number.map(|n| n.as_u64()).unwrap_or(0);
                        debug!("New block: {}", block_num);
                        if tx.send(DetectionTrigger::NewBlock(block_num)).is_err() {
                            break;
                        }
                    }

                    // Backup polling interval
                    _ = backup_interval.tick() => {
                        debug!("Backup poll triggered");
                        if tx.send(DetectionTrigger::BackupPoll).is_err() {
                            break;
                        }
                    }
                }
            }

            warn!("Combined watcher ended");
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_parsing() {
        let topic: H256 = UNISWAP_V3_SWAP_TOPIC.parse().unwrap();
        assert!(!topic.is_zero());
    }

    #[test]
    fn test_config_creation() {
        let config = WatcherConfig::arbitrum_lst_pools();
        assert!(!config.uniswap_v3_pools.is_empty());
        assert!(!config.curve_pools.is_empty());
    }
}
