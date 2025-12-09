use ethers::prelude::*;
use ethers::abi::{encode, Token, Tokenize};
use ethers::types::{Bytes, Address, U256};
use std::sync::Arc;
use tracing::{debug, warn};

use super::cache::{Quote, Venue};
use crate::rpc::WsClient;

// Multicall3 ABI
abigen!(
    Multicall3,
    r#"[
        struct Call3 { address target; bool allowFailure; bytes callData; }
        struct Result { bool success; bytes returnData; }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData)
    ]"#
);

// Curve Pool ABI
abigen!(
    CurvePool,
    r#"[
        function get_dy(int128 i, int128 j, uint256 dx) external view returns (uint256)
        function balances(uint256 i) external view returns (uint256)
    ]"#
);

// Balancer Vault ABI for queries
abigen!(
    BalancerVault,
    r#"[
        function queryBatchSwap(uint8 kind, tuple(bytes32 poolId, uint256 assetInIndex, uint256 assetOutIndex, uint256 amount, bytes userData)[] swaps, address[] assets, tuple(address sender, bool fromInternalBalance, address recipient, bool toInternalBalance) funds) external returns (int256[] assetDeltas)
    ]"#
);

// UniswapV3 Quoter ABI
abigen!(
    UniswapQuoter,
    r#"[
        function quoteExactInputSingle(tuple(address tokenIn, address tokenOut, uint256 amountIn, uint24 fee, uint160 sqrtPriceLimitX96) params) external returns (uint256 amountOut, uint160 sqrtPriceX96After, uint32 initializedTicksCrossed, uint256 gasEstimate)
    ]"#
);

#[derive(Debug, Clone)]
pub struct VenueAddresses {
    pub multicall3: Address,
    pub curve_steth: Address,
    pub curve_reth: Address,
    pub balancer_vault: Address,
    pub uniswap_quoter: Address,
    pub weth: Address,
}

pub struct MulticallQuoter {
    addresses: VenueAddresses,
}

#[derive(Debug, Clone)]
pub struct TokenQuotes {
    pub token: Address,
    pub token_name: String,
    pub quotes: Vec<(Venue, Quote)>,
}

impl MulticallQuoter {
    pub fn new(addresses: VenueAddresses) -> Self {
        Self { addresses }
    }
    
    /// Fetch all quotes for multiple tokens in a SINGLE RPC call
    /// This is the key to speed - one call gets everything
    pub async fn fetch_all_quotes(
        &self,
        client: Arc<WsClient>,
        tokens: &[(Address, String)], // (token_address, name)
        amount: U256, // Amount of WETH to quote
    ) -> eyre::Result<Vec<TokenQuotes>> {
        let multicall = Multicall3::new(self.addresses.multicall3, client.clone());
        
        let mut calls: Vec<Call3> = Vec::new();
        let mut call_mapping: Vec<(usize, Address, Venue, bool)> = Vec::new(); // (call_idx, token, venue, is_buy)
        
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        
        for (token, name) in tokens {
            // ===== CURVE QUOTES =====
            // Only for supported tokens (stETH, rETH)
            if let Some(curve_pool) = self.get_curve_pool(*token) {
                // Buy LST (ETH -> LST): get_dy(0, 1, amount)
                let buy_data = self.encode_curve_get_dy(0, 1, amount);
                calls.push(Call3 {
                    target: curve_pool,
                    allow_failure: true,
                    call_data: buy_data,
                });
                call_mapping.push((calls.len() - 1, *token, Venue::Curve, true));
                
                // Sell LST (LST -> ETH): get_dy(1, 0, amount)
                let sell_data = self.encode_curve_get_dy(1, 0, amount);
                calls.push(Call3 {
                    target: curve_pool,
                    allow_failure: true,
                    call_data: sell_data,
                });
                call_mapping.push((calls.len() - 1, *token, Venue::Curve, false));
            }
            
            // ===== UNISWAP V3 QUOTES =====
            // Buy LST (WETH -> LST)
            let uni_buy_data = self.encode_uniswap_quote(
                self.addresses.weth,
                *token,
                amount,
                500, // 0.05% fee tier (common for LSTs)
            );
            calls.push(Call3 {
                target: self.addresses.uniswap_quoter,
                allow_failure: true,
                call_data: uni_buy_data,
            });
            call_mapping.push((calls.len() - 1, *token, Venue::UniswapV3, true));
            
            // Sell LST (LST -> WETH)
            let uni_sell_data = self.encode_uniswap_quote(
                *token,
                self.addresses.weth,
                amount,
                500,
            );
            calls.push(Call3 {
                target: self.addresses.uniswap_quoter,
                allow_failure: true,
                call_data: uni_sell_data,
            });
            call_mapping.push((calls.len() - 1, *token, Venue::UniswapV3, false));
            
            // Also try 0.3% fee tier
            let uni_buy_data_30 = self.encode_uniswap_quote(
                self.addresses.weth,
                *token,
                amount,
                3000,
            );
            calls.push(Call3 {
                target: self.addresses.uniswap_quoter,
                allow_failure: true,
                call_data: uni_buy_data_30,
            });
            call_mapping.push((calls.len() - 1, *token, Venue::UniswapV3, true));
        }
        
        // Execute single multicall
        debug!("Executing multicall with {} calls", calls.len());
        let results = multicall.aggregate_3(calls).call().await?;
        
        // Parse results
        let mut token_quotes: std::collections::HashMap<Address, TokenQuotes> = 
            std::collections::HashMap::new();
        
        for (token, name) in tokens {
            token_quotes.insert(*token, TokenQuotes {
                token: *token,
                token_name: name.clone(),
                quotes: Vec::new(),
            });
        }
        
        // Aggregate quotes by venue (take best quote per venue)
        let mut venue_quotes: std::collections::HashMap<(Address, Venue), (U256, U256)> = 
            std::collections::HashMap::new();
        
        for (idx, token, venue, is_buy) in &call_mapping {
            if let Some(result) = results.get(*idx) {
                if result.success && !result.return_data.is_empty() {
                    if let Ok(amount_out) = self.decode_quote_result(&result.return_data, *venue) {
                        let key = (*token, *venue);
                        let entry = venue_quotes.entry(key).or_insert((U256::zero(), U256::zero()));
                        
                        if *is_buy {
                            // Take best (highest) buy amount
                            if amount_out > entry.0 {
                                entry.0 = amount_out;
                            }
                        } else {
                            // Take best (highest) sell amount
                            if amount_out > entry.1 {
                                entry.1 = amount_out;
                            }
                        }
                    }
                }
            }
        }
        
        // Convert to final format
        for ((token, venue), (buy_amount, sell_amount)) in venue_quotes {
            if buy_amount > U256::zero() || sell_amount > U256::zero() {
                if let Some(tq) = token_quotes.get_mut(&token) {
                    tq.quotes.push((venue, Quote {
                        buy_amount,
                        sell_amount,
                        liquidity: U256::zero(), // Could add liquidity queries
                        timestamp_ms,
                    }));
                }
            }
        }
        
        Ok(token_quotes.into_values().collect())
    }
    
    fn get_curve_pool(&self, token: Address) -> Option<Address> {
        // stETH pool
        if token == "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84".parse().unwrap() {
            return Some(self.addresses.curve_steth);
        }
        // rETH pool
        if token == "0xae78736Cd615f374D3085123A210448E74Fc6393".parse().unwrap() {
            return Some(self.addresses.curve_reth);
        }
        None
    }
    
    fn encode_curve_get_dy(&self, i: i128, j: i128, dx: U256) -> Bytes {
        // get_dy(int128,int128,uint256)
        let selector = ethers::utils::id("get_dy(int128,int128,uint256)");
        let mut data = selector[..4].to_vec();
        
        // Encode int128 as 32-byte signed integer
        let i_bytes = if i >= 0 {
            let mut b = [0u8; 32];
            b[31] = i as u8;
            b
        } else {
            [0xffu8; 32] // -1
        };
        
        let j_bytes = if j >= 0 {
            let mut b = [0u8; 32];
            b[31] = j as u8;
            b
        } else {
            [0xffu8; 32]
        };
        
        data.extend_from_slice(&i_bytes);
        data.extend_from_slice(&j_bytes);
        
        let mut dx_bytes = [0u8; 32];
        dx.to_big_endian(&mut dx_bytes);
        data.extend_from_slice(&dx_bytes);
        
        Bytes::from(data)
    }
    
    fn encode_uniswap_quote(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        fee: u32,
    ) -> Bytes {
        // quoteExactInputSingle((address,address,uint256,uint24,uint160))
        let selector = ethers::utils::id("quoteExactInputSingle((address,address,uint256,uint24,uint160))");
        let mut data = selector[..4].to_vec();
        
        // Encode tuple as packed parameters
        // Offset to tuple data (32 bytes)
        data.extend_from_slice(&[0u8; 31]);
        data.push(0x20);
        
        // tokenIn (address - 32 bytes, left-padded)
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(token_in.as_bytes());
        
        // tokenOut
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(token_out.as_bytes());
        
        // amountIn (uint256)
        let mut amount_bytes = [0u8; 32];
        amount_in.to_big_endian(&mut amount_bytes);
        data.extend_from_slice(&amount_bytes);
        
        // fee (uint24 - 32 bytes, left-padded)
        let mut fee_bytes = [0u8; 32];
        fee_bytes[29] = (fee >> 16) as u8;
        fee_bytes[30] = (fee >> 8) as u8;
        fee_bytes[31] = fee as u8;
        data.extend_from_slice(&fee_bytes);
        
        // sqrtPriceLimitX96 (uint160 = 0)
        data.extend_from_slice(&[0u8; 32]);
        
        Bytes::from(data)
    }
    
    fn decode_quote_result(&self, data: &[u8], venue: Venue) -> eyre::Result<U256> {
        match venue {
            Venue::Curve => {
                // Returns uint256 directly
                if data.len() >= 32 {
                    Ok(U256::from_big_endian(&data[..32]))
                } else {
                    Err(eyre::eyre!("Invalid Curve response"))
                }
            }
            Venue::UniswapV3 => {
                // Returns (uint256 amountOut, uint160, uint32, uint256)
                if data.len() >= 32 {
                    Ok(U256::from_big_endian(&data[..32]))
                } else {
                    Err(eyre::eyre!("Invalid UniswapV3 response"))
                }
            }
            Venue::Balancer => {
                // Balancer queryBatchSwap returns int256[]
                // First value is the delta (negative = received)
                if data.len() >= 64 {
                    // Skip array offset and length, get first int256
                    let value = U256::from_big_endian(&data[32..64]);
                    Ok(value)
                } else {
                    Err(eyre::eyre!("Invalid Balancer response"))
                }
            }
            _ => Err(eyre::eyre!("Unsupported venue")),
        }
    }
}
