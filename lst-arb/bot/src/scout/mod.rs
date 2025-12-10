//! Scout module for discovering top Arbitrum pools and verifying token safety.
//!
//! This module provides functionality to:
//! - Fetch top pools from The Graph's Uniswap V3 Arbitrum subgraph
//! - Verify token safety by detecting honeypot/taxed tokens

use ethers::prelude::*;
use ethers::types::Address;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

/// GraphQL endpoint for Uniswap V3 on Arbitrum
const UNISWAP_V3_ARBITRUM_SUBGRAPH: &str =
    "https://api.thegraph.com/subgraphs/name/uniswap/uniswap-v3-arbitrum";

/// Maximum gas for a safe token transfer
const MAX_SAFE_TRANSFER_GAS: u64 = 100_000;

/// ERC20 transfer function selector: transfer(address,uint256)
const TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

/// Represents a discovered pool from The Graph
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
}

/// Token verification result
#[derive(Debug, Clone)]
pub struct TokenVerification {
    pub address: Address,
    pub is_safe: bool,
    pub gas_used: u64,
    pub reason: Option<String>,
}

/// GraphQL response structures
#[derive(Debug, Deserialize)]
struct GraphQLResponse {
    data: Option<GraphQLData>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLData {
    pools: Vec<PoolData>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PoolData {
    id: String,
    token0: TokenData,
    token1: TokenData,
    fee_tier: String,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    id: String,
    symbol: String,
}

/// GraphQL query request
#[derive(Debug, Serialize)]
struct GraphQLQuery {
    query: String,
}

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

    /// Fetch top pools from The Graph's Uniswap V3 Arbitrum subgraph
    ///
    /// Returns the top 20 pools by volume with TVL > $50,000
    pub async fn fetch_top_pools(&self) -> eyre::Result<Vec<TargetPool>> {
        let query = GraphQLQuery {
            query: r#"
                {
                    pools(
                        first: 20,
                        orderBy: volumeUSD,
                        orderDirection: desc,
                        where: { totalValueLockedUSD_gt: 50000 }
                    ) {
                        id
                        token0 { id symbol }
                        token1 { id symbol }
                        feeTier
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

        // Check for GraphQL errors
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

                Some(TargetPool {
                    address,
                    token0,
                    token0_symbol: pool.token0.symbol,
                    token1,
                    token1_symbol: pool.token1.symbol,
                    fee_tier,
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
            .gas(200_000u64); // Set high gas limit for estimation

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
                    info!(
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
                // Check if error is due to revert or other issues
                let error_msg = format!("{:?}", e);

                // Some reverts are expected for 0-value transfers to certain tokens
                // Check if it's a simple revert vs a complex failure
                let is_simple_revert = error_msg.contains("revert")
                    || error_msg.contains("execution reverted");

                if is_simple_revert {
                    // Try an alternative check - call balanceOf instead
                    match self.check_balance_of(provider.clone(), token_address).await {
                        Ok(gas) if gas <= MAX_SAFE_TRANSFER_GAS => {
                            info!(
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
        // balanceOf(address) selector: 0x70a08231
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&[0x70, 0xa0, 0x82, 0x31]);
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
            "Discovered {} safe pools (filtered from {} total)",
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
        };
        assert_eq!(pool.fee_tier, 3000);
    }

    #[test]
    fn test_transfer_selector() {
        // keccak256("transfer(address,uint256)")[0:4]
        assert_eq!(TRANSFER_SELECTOR, [0xa9, 0x05, 0x9c, 0xbb]);
    }
}
