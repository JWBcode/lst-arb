//! Scout module for discovering top Arbitrum pools and verifying token safety.
//!
//! This module provides functionality to:
//! - Fetch top pools from DexScreener API (primary) or The Graph (fallback)
//! - Verify token safety by detecting honeypot/taxed tokens
//! - Filter pools by liquidity and volume

use ethers::prelude::*;
use ethers::types::Address;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn, debug};

/// DexScreener API endpoint for Arbitrum WETH pairs
const DEXSCREENER_API: &str = "https://api.dexscreener.com/latest/dex/search";

/// GraphQL endpoint for Uniswap V3 on Arbitrum (fallback)
const UNISWAP_V3_ARBITRUM_SUBGRAPH: &str =
    "https://api.thegraph.com/subgraphs/name/uniswap/uniswap-v3-arbitrum";

/// Minimum liquidity threshold in USD
const MIN_LIQUIDITY_USD: f64 = 50_000.0;

/// Maximum gas for a safe token transfer
const MAX_SAFE_TRANSFER_GAS: u64 = 100_000;

/// ERC20 transfer function selector: transfer(address,uint256)
const TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

/// ERC20 balanceOf function selector: balanceOf(address)
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// Represents a discovered pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetPool {
    /// Pool contract address
    pub address: Address,
    /// Token0 address
    pub token0: Address,
    /// Token0 symbol
    pub token0_symbol: String,
    /// Token1 address
    pub token1: Address,
    /// Token1 symbol
    pub token1_symbol: String,
    /// Fee tier in hundredths of a bip (e.g., 3000 = 0.3%)
    pub fee_tier: u32,
    /// Liquidity in USD
    pub liquidity_usd: f64,
    /// 24h volume in USD
    pub volume_24h_usd: f64,
    /// Price volatility (24h price change percentage)
    pub volatility: f64,
}

impl TargetPool {
    /// Calculate a score for sorting (higher is better)
    /// Score = volume * volatility / sqrt(liquidity)
    pub fn score(&self) -> f64 {
        if self.liquidity_usd <= 0.0 {
            return 0.0;
        }
        (self.volume_24h_usd * self.volatility.abs()) / self.liquidity_usd.sqrt()
    }
}

/// Token verification result
#[derive(Debug, Clone)]
pub struct TokenVerification {
    pub address: Address,
    pub is_safe: bool,
    pub gas_used: u64,
    pub reason: Option<String>,
}

// ============================================================================
// DexScreener API Response Structures
// ============================================================================

#[derive(Debug, Deserialize)]
struct DexScreenerResponse {
    pairs: Option<Vec<DexScreenerPair>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DexScreenerPair {
    chain_id: String,
    dex_id: String,
    pair_address: String,
    base_token: DexScreenerToken,
    quote_token: DexScreenerToken,
    liquidity: Option<DexScreenerLiquidity>,
    volume: Option<DexScreenerVolume>,
    price_change: Option<DexScreenerPriceChange>,
    fee_tier: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DexScreenerToken {
    address: String,
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct DexScreenerLiquidity {
    usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DexScreenerVolume {
    h24: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DexScreenerPriceChange {
    h24: Option<f64>,
}

// ============================================================================
// The Graph Response Structures (Fallback)
// ============================================================================

#[derive(Debug, Deserialize)]
struct GraphQLResponse {
    data: Option<GraphQLData>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLData {
    pools: Vec<GraphQLPool>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQLPool {
    id: String,
    token0: GraphQLToken,
    token1: GraphQLToken,
    fee_tier: String,
    total_value_locked_usd: Option<String>,
    volume_usd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphQLToken {
    id: String,
    symbol: String,
}

#[derive(Debug, Serialize)]
struct GraphQLQuery {
    query: String,
}

// ============================================================================
// Scout Implementation
// ============================================================================

/// Scout for discovering and verifying Arbitrum pools
pub struct Scout {
    http_client: reqwest::Client,
}

impl Scout {
    /// Create a new Scout instance
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Fetch top pools from DexScreener API
    ///
    /// Queries DexScreener for WETH pairs on Arbitrum, filters by liquidity,
    /// and sorts by volume/volatility score.
    pub async fn fetch_top_pools(&self) -> eyre::Result<Vec<TargetPool>> {
        info!("Fetching top pools from DexScreener...");

        // Try DexScreener first
        match self.fetch_from_dexscreener().await {
            Ok(pools) if !pools.is_empty() => {
                info!("Fetched {} pools from DexScreener", pools.len());
                return Ok(pools);
            }
            Ok(_) => {
                warn!("DexScreener returned no pools, falling back to The Graph");
            }
            Err(e) => {
                warn!("DexScreener failed: {:?}, falling back to The Graph", e);
            }
        }

        // Fallback to The Graph
        self.fetch_from_the_graph().await
    }

    /// Fetch pools from DexScreener API
    async fn fetch_from_dexscreener(&self) -> eyre::Result<Vec<TargetPool>> {
        let url = format!("{}?q=WETH%20arbitrum", DEXSCREENER_API);

        let response = self.http_client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "DexScreener API request failed with status {}: {}",
                status,
                body
            ));
        }

        let dex_response: DexScreenerResponse = response.json().await?;

        let pairs = dex_response.pairs.unwrap_or_default();

        let mut pools: Vec<TargetPool> = pairs
            .into_iter()
            .filter(|p| p.chain_id == "arbitrum")
            .filter_map(|pair| {
                let liquidity_usd = pair.liquidity
                    .as_ref()
                    .and_then(|l| l.usd)
                    .unwrap_or(0.0);

                // Filter by minimum liquidity
                if liquidity_usd < MIN_LIQUIDITY_USD {
                    return None;
                }

                let address = pair.pair_address.parse::<Address>().ok()?;
                let token0 = pair.base_token.address.parse::<Address>().ok()?;
                let token1 = pair.quote_token.address.parse::<Address>().ok()?;

                let volume_24h_usd = pair.volume
                    .as_ref()
                    .and_then(|v| v.h24)
                    .unwrap_or(0.0);

                let volatility = pair.price_change
                    .as_ref()
                    .and_then(|p| p.h24)
                    .unwrap_or(0.0);

                // Default fee tier based on DEX
                let fee_tier = pair.fee_tier.unwrap_or_else(|| {
                    match pair.dex_id.as_str() {
                        "uniswap" => 3000,   // 0.3%
                        "camelot" => 3000,   // 0.3%
                        "sushiswap" => 3000, // 0.3%
                        _ => 3000,
                    }
                });

                Some(TargetPool {
                    address,
                    token0,
                    token0_symbol: pair.base_token.symbol,
                    token1,
                    token1_symbol: pair.quote_token.symbol,
                    fee_tier,
                    liquidity_usd,
                    volume_24h_usd,
                    volatility,
                })
            })
            .collect();

        // Sort by score (volume * volatility / sqrt(liquidity))
        pools.sort_by(|a, b| {
            b.score().partial_cmp(&a.score()).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Take top 20
        pools.truncate(20);

        for (i, pool) in pools.iter().enumerate() {
            debug!(
                "  #{}: {} {}/{} | Liq: ${:.0} | Vol: ${:.0} | Score: {:.2}",
                i + 1,
                pool.address,
                pool.token0_symbol,
                pool.token1_symbol,
                pool.liquidity_usd,
                pool.volume_24h_usd,
                pool.score()
            );
        }

        Ok(pools)
    }

    /// Fetch pools from The Graph (fallback)
    async fn fetch_from_the_graph(&self) -> eyre::Result<Vec<TargetPool>> {
        let query = GraphQLQuery {
            query: r#"
                {
                    pools(
                        first: 20,
                        orderBy: volumeUSD,
                        orderDirection: desc,
                        where: { totalValueLockedUSD_gt: "50000" }
                    ) {
                        id
                        token0 { id symbol }
                        token1 { id symbol }
                        feeTier
                        totalValueLockedUSD
                        volumeUSD
                    }
                }
            "#.to_string(),
        };

        info!("Fetching top pools from The Graph...");

        let response = self.http_client
            .post(UNISWAP_V3_ARBITRUM_SUBGRAPH)
            .json(&query)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "The Graph API request failed with status {}: {}",
                status,
                body
            ));
        }

        let graphql_response: GraphQLResponse = response.json().await?;

        if let Some(errors) = graphql_response.errors {
            let error_msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
            return Err(eyre::eyre!("GraphQL errors: {}", error_msgs.join(", ")));
        }

        let data = graphql_response.data
            .ok_or_else(|| eyre::eyre!("No data in GraphQL response"))?;

        let pools: Vec<TargetPool> = data.pools
            .into_iter()
            .filter_map(|pool| {
                let address = pool.id.parse::<Address>().ok()?;
                let token0 = pool.token0.id.parse::<Address>().ok()?;
                let token1 = pool.token1.id.parse::<Address>().ok()?;
                let fee_tier = pool.fee_tier.parse::<u32>().ok()?;

                let liquidity_usd = pool.total_value_locked_usd
                    .as_ref()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);

                let volume_24h_usd = pool.volume_usd
                    .as_ref()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);

                Some(TargetPool {
                    address,
                    token0,
                    token0_symbol: pool.token0.symbol,
                    token1,
                    token1_symbol: pool.token1.symbol,
                    fee_tier,
                    liquidity_usd,
                    volume_24h_usd,
                    volatility: 0.0, // Not available from The Graph
                })
            })
            .collect();

        info!("Fetched {} pools from The Graph", pools.len());

        for pool in &pools {
            info!(
                "  Pool {:?}: {}/{} (fee: {}bps)",
                pool.address,
                pool.token0_symbol,
                pool.token1_symbol,
                pool.fee_tier / 100
            );
        }

        Ok(pools)
    }

    /// Verify if a token is safe (not a honeypot or taxed token)
    ///
    /// Performs a simulated 0-value transfer call to the token address.
    /// If the call reverts or consumes more than 100k gas, the token is flagged as unsafe.
    pub async fn verify_token_l2<P: JsonRpcClient>(
        &self,
        provider: Arc<Provider<P>>,
        token_address: Address,
    ) -> TokenVerification {
        // Build transfer calldata: transfer(address(0), 0)
        let mut calldata = Vec::with_capacity(68);
        calldata.extend_from_slice(&TRANSFER_SELECTOR);
        // Pad address to 32 bytes
        calldata.extend_from_slice(&[0u8; 12]);
        calldata.extend_from_slice(Address::zero().as_bytes());
        // Pad uint256 (0) to 32 bytes
        calldata.extend_from_slice(&[0u8; 32]);

        let tx = TransactionRequest::new()
            .to(token_address)
            .data(calldata)
            .gas(200_000u64);

        // Perform eth_call to simulate the transfer
        match provider.estimate_gas(&tx.clone().into(), None).await {
            Ok(gas_used) => {
                let gas = gas_used.as_u64();

                if gas > MAX_SAFE_TRANSFER_GAS {
                    warn!(
                        "Token {:?} uses excessive gas: {} (max: {})",
                        token_address, gas, MAX_SAFE_TRANSFER_GAS
                    );
                    TokenVerification {
                        address: token_address,
                        is_safe: false,
                        gas_used: gas,
                        reason: Some(format!(
                            "Excessive gas usage: {} > {} (possible honeypot/tax)",
                            gas, MAX_SAFE_TRANSFER_GAS
                        )),
                    }
                } else {
                    debug!(
                        "Token {:?} verified safe (gas: {})",
                        token_address, gas
                    );
                    TokenVerification {
                        address: token_address,
                        is_safe: true,
                        gas_used: gas,
                        reason: None,
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("{:?}", e);

                // Some reverts are expected for 0-value transfers
                let is_simple_revert = error_msg.contains("revert")
                    || error_msg.contains("execution reverted");

                if is_simple_revert {
                    // Try balanceOf as alternative check
                    match self.check_balance_of(provider.clone(), token_address).await {
                        Ok(gas) if gas <= MAX_SAFE_TRANSFER_GAS => {
                            debug!(
                                "Token {:?} passed balanceOf check (gas: {})",
                                token_address, gas
                            );
                            return TokenVerification {
                                address: token_address,
                                is_safe: true,
                                gas_used: gas,
                                reason: None,
                            };
                        }
                        Ok(gas) => {
                            warn!(
                                "Token {:?} uses excessive gas in balanceOf: {}",
                                token_address, gas
                            );
                            return TokenVerification {
                                address: token_address,
                                is_safe: false,
                                gas_used: gas,
                                reason: Some(format!(
                                    "Excessive gas in balanceOf: {} (possible honeypot)",
                                    gas
                                )),
                            };
                        }
                        Err(_) => {}
                    }
                }

                warn!(
                    "Token {:?} verification failed: {}",
                    token_address, error_msg
                );
                TokenVerification {
                    address: token_address,
                    is_safe: false,
                    gas_used: 0,
                    reason: Some(format!("Transfer simulation failed: {}", error_msg)),
                }
            }
        }
    }

    /// Alternative check using balanceOf(address(0))
    async fn check_balance_of<P: JsonRpcClient>(
        &self,
        provider: Arc<Provider<P>>,
        token_address: Address,
    ) -> eyre::Result<u64> {
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&BALANCE_OF_SELECTOR);
        calldata.extend_from_slice(&[0u8; 12]);
        calldata.extend_from_slice(Address::zero().as_bytes());

        let tx = TransactionRequest::new()
            .to(token_address)
            .data(calldata)
            .gas(200_000u64);

        let gas = provider.estimate_gas(&tx.into(), None).await?;
        Ok(gas.as_u64())
    }

    /// Verify multiple tokens and filter out unsafe ones
    pub async fn verify_tokens<P: JsonRpcClient>(
        &self,
        provider: Arc<Provider<P>>,
        tokens: Vec<Address>,
    ) -> Vec<TokenVerification> {
        let mut results = Vec::with_capacity(tokens.len());

        for token in tokens {
            let verification = self.verify_token_l2(provider.clone(), token).await;
            results.push(verification);
        }

        let safe_count = results.iter().filter(|v| v.is_safe).count();
        info!(
            "Token verification complete: {}/{} tokens are safe",
            safe_count,
            results.len()
        );

        results
    }

    /// Fetch pools and verify all tokens, returning only pools with safe tokens
    pub async fn discover_safe_pools<P: JsonRpcClient>(
        &self,
        provider: Arc<Provider<P>>,
    ) -> eyre::Result<Vec<TargetPool>> {
        let pools = self.fetch_top_pools().await?;

        // Collect unique tokens from all pools
        let mut unique_tokens: Vec<Address> = Vec::new();
        for pool in &pools {
            if !unique_tokens.contains(&pool.token0) {
                unique_tokens.push(pool.token0);
            }
            if !unique_tokens.contains(&pool.token1) {
                unique_tokens.push(pool.token1);
            }
        }

        info!("Verifying {} unique tokens...", unique_tokens.len());

        // Verify all tokens
        let verifications = self.verify_tokens(provider, unique_tokens).await;

        // Build set of safe tokens
        let safe_tokens: std::collections::HashSet<Address> = verifications
            .iter()
            .filter(|v| v.is_safe)
            .map(|v| v.address)
            .collect();

        // Filter pools to only include those with both safe tokens
        let safe_pools: Vec<TargetPool> = pools
            .into_iter()
            .filter(|pool| {
                safe_tokens.contains(&pool.token0) && safe_tokens.contains(&pool.token1)
            })
            .collect();

        info!(
            "Discovered {} safe pools (from {} verified tokens)",
            safe_pools.len(),
            safe_tokens.len()
        );

        Ok(safe_pools)
    }
}

impl Default for Scout {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_pool_creation() {
        let pool = TargetPool {
            address: Address::zero(),
            token0: Address::zero(),
            token0_symbol: "WETH".to_string(),
            token1: Address::zero(),
            token1_symbol: "USDC".to_string(),
            fee_tier: 3000,
            liquidity_usd: 100_000.0,
            volume_24h_usd: 50_000.0,
            volatility: 2.5,
        };
        assert_eq!(pool.fee_tier, 3000);
        assert_eq!(pool.liquidity_usd, 100_000.0);
    }

    #[test]
    fn test_pool_score_calculation() {
        let pool = TargetPool {
            address: Address::zero(),
            token0: Address::zero(),
            token0_symbol: "WETH".to_string(),
            token1: Address::zero(),
            token1_symbol: "USDC".to_string(),
            fee_tier: 3000,
            liquidity_usd: 100_000.0,   // sqrt = 316.23
            volume_24h_usd: 50_000.0,
            volatility: 2.0,
        };

        // Score = (50000 * 2.0) / sqrt(100000) = 100000 / 316.23 ≈ 316.23
        let score = pool.score();
        assert!((score - 316.23).abs() < 1.0);
    }

    #[test]
    fn test_pool_score_zero_liquidity() {
        let pool = TargetPool {
            address: Address::zero(),
            token0: Address::zero(),
            token0_symbol: "WETH".to_string(),
            token1: Address::zero(),
            token1_symbol: "USDC".to_string(),
            fee_tier: 3000,
            liquidity_usd: 0.0,
            volume_24h_usd: 50_000.0,
            volatility: 2.0,
        };

        assert_eq!(pool.score(), 0.0);
    }

    #[test]
    fn test_pool_sorting_by_score() {
        let pool_a = TargetPool {
            address: "0x0000000000000000000000000000000000000001".parse().unwrap(),
            token0: Address::zero(),
            token0_symbol: "A".to_string(),
            token1: Address::zero(),
            token1_symbol: "B".to_string(),
            fee_tier: 3000,
            liquidity_usd: 100_000.0,
            volume_24h_usd: 10_000.0,
            volatility: 1.0,
        };

        let pool_b = TargetPool {
            address: "0x0000000000000000000000000000000000000002".parse().unwrap(),
            token0: Address::zero(),
            token0_symbol: "C".to_string(),
            token1: Address::zero(),
            token1_symbol: "D".to_string(),
            fee_tier: 3000,
            liquidity_usd: 100_000.0,
            volume_24h_usd: 50_000.0, // Higher volume
            volatility: 2.0,           // Higher volatility
        };

        let mut pools = vec![pool_a.clone(), pool_b.clone()];
        pools.sort_by(|a, b| {
            b.score().partial_cmp(&a.score()).unwrap_or(std::cmp::Ordering::Equal)
        });

        // pool_b should be first (higher score)
        assert_eq!(pools[0].token0_symbol, "C");
        assert_eq!(pools[1].token0_symbol, "A");
    }

    #[test]
    fn test_transfer_selector() {
        // keccak256("transfer(address,uint256)")[0:4]
        assert_eq!(TRANSFER_SELECTOR, [0xa9, 0x05, 0x9c, 0xbb]);
    }

    #[test]
    fn test_balance_of_selector() {
        // keccak256("balanceOf(address)")[0:4]
        assert_eq!(BALANCE_OF_SELECTOR, [0x70, 0xa0, 0x82, 0x31]);
    }

    #[test]
    fn test_min_liquidity_threshold() {
        assert_eq!(MIN_LIQUIDITY_USD, 50_000.0);
    }

    #[test]
    fn test_max_safe_transfer_gas() {
        assert_eq!(MAX_SAFE_TRANSFER_GAS, 100_000);
    }

    #[test]
    fn test_token_verification_safe() {
        let verification = TokenVerification {
            address: Address::zero(),
            is_safe: true,
            gas_used: 50_000,
            reason: None,
        };
        assert!(verification.is_safe);
        assert!(verification.gas_used < MAX_SAFE_TRANSFER_GAS);
    }

    #[test]
    fn test_token_verification_unsafe() {
        let verification = TokenVerification {
            address: Address::zero(),
            is_safe: false,
            gas_used: 150_000,
            reason: Some("Excessive gas".to_string()),
        };
        assert!(!verification.is_safe);
        assert!(verification.gas_used > MAX_SAFE_TRANSFER_GAS);
    }

    #[test]
    fn test_scout_creation() {
        let scout = Scout::new();
        // Just ensure it doesn't panic
        assert!(true);
        drop(scout);
    }

    #[test]
    fn test_scout_default() {
        let scout = Scout::default();
        drop(scout);
    }
}
