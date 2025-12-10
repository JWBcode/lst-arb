use ethers::types::{Address, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub rpc: RpcConfig,
    pub tokens: TokenConfig,
    pub venues: VenueConfig,
    pub strategy: StrategyConfig,
    pub execution: ExecutionConfig,
    pub monitoring: MonitoringConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    pub primary: String,
    pub backup1: String,
    pub backup2: String,
    pub health_check_interval_ms: u64,
    pub max_latency_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    // LSTs on Arbitrum (stETH not available on L2)
    pub wsteth: String,
    pub reth: String,
    pub cbeth: String,
    // LRTs
    pub weeth: String,
    pub ezeth: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VenueConfig {
    pub curve_steth_pool: String,
    pub curve_reth_pool: String,
    pub balancer_vault: String,
    pub uniswap_quoter: String,
    pub uniswap_router: String,
    pub multicall3: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    pub min_spread_bps: u64,
    pub min_profit_wei: String,
    // max_trade_size_eth removed - now determined by convex optimization solver
    pub poll_interval_ms: u64,
    pub enabled_tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    pub use_flashbots: bool,
    pub flashbots_relay: String,
    pub max_gas_price_gwei: u64,
    pub max_priority_fee_gwei: u64,
    pub gas_buffer_percent: u64,
    pub arb_contract: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MonitoringConfig {
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub log_level: String,
}

impl Config {
    pub fn load(path: &str) -> eyre::Result<Self> {
        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
    
    pub fn load_or_default() -> Self {
        Self::load("config.toml").unwrap_or_else(|_| Self::default())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rpc: RpcConfig {
                // Arbitrum One RPC endpoints
                primary: std::env::var("RPC_URL_PRIMARY")
                    .unwrap_or_else(|_| "https://arb1.arbitrum.io/rpc".into()),
                backup1: std::env::var("RPC_URL_BACKUP1")
                    .unwrap_or_else(|_| "https://arb1.arbitrum.io/rpc".into()),
                backup2: std::env::var("RPC_URL_BACKUP2")
                    .unwrap_or_else(|_| "https://arbitrum-mainnet.infura.io/v3/demo".into()),
                health_check_interval_ms: 5000,
                max_latency_ms: 100,
            },
            tokens: TokenConfig {
                // Arbitrum token addresses (stETH not available on L2)
                wsteth: "0x5979D7b546E38E41137eFe97697CBca551Db098E".into(),
                reth: "0xEC70Dcb4A1EfA46b8F2D97C310C9c4790bA5ffA8".into(),
                cbeth: "0x1DEBd73E752bEaF79865Fd6446b0c970EaE7732f".into(),
                weeth: "0x35751007a407ca6feffe80b3cb397736d2cf4dbe".into(),
                ezeth: "0x2416092f143378750bb29b79ed961ab195cceea5".into(),
            },
            venues: VenueConfig {
                // Arbitrum venue addresses
                curve_steth_pool: "0x6eB2dc694eB516B16Dc9d7671f465248B71E9091".into(), // wstETH/ETH NG Pool
                curve_reth_pool: "0x0000000000000000000000000000000000000000".into(), // Low liquidity on Arb
                balancer_vault: "0xBA12222222228d8Ba445958a75a0704d566BF2C8".into(),
                uniswap_quoter: "0x61fFE014bA17989E743c5F6cB21bF9697530B21e".into(),
                uniswap_router: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                multicall3: "0xcA11bde05977b3631167028862bE2a173976CA11".into(),
            },
            strategy: StrategyConfig {
                min_spread_bps: 20,
                min_profit_wei: "1000000000000000".into(), // 0.001 ETH for low-capital L2 operation
                // Trade size determined by convex optimization solver with 90% liquidity clamping
                poll_interval_ms: 200,
                enabled_tokens: vec![
                    "wsteth".into(),
                    "reth".into(),
                    "weeth".into(),
                    "ezeth".into(),
                ],
            },
            execution: ExecutionConfig {
                // Arbitrum uses FIFO sequencer - no Flashbots
                use_flashbots: false,
                flashbots_relay: "".into(),
                max_gas_price_gwei: 2, // Arbitrum L2 gas is typically 0.1 gwei
                max_priority_fee_gwei: 0,
                gas_buffer_percent: 20,
                arb_contract: std::env::var("ARB_CONTRACT").unwrap_or_default(),
            },
            monitoring: MonitoringConfig {
                telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").ok(),
                telegram_chat_id: std::env::var("TELEGRAM_CHAT_ID").ok(),
                log_level: "info".into(),
            },
        }
    }
}

// Parsed addresses for runtime use
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    pub weth: Address,
    pub tokens: HashMap<String, Address>,
    pub venues: ParsedVenues,
    pub arb_contract: Address,
    pub min_spread_bps: u64,
    pub min_profit: U256,
    // max_trade_size removed - determined by convex optimization solver
}

#[derive(Debug, Clone)]
pub struct ParsedVenues {
    pub curve_steth: Address,
    pub curve_reth: Address,
    pub balancer_vault: Address,
    pub uniswap_quoter: Address,
    pub uniswap_router: Address,
    pub multicall3: Address,
}

impl ParsedConfig {
    pub fn from_config(config: &Config) -> eyre::Result<Self> {
        let mut tokens = HashMap::new();
        // Arbitrum token addresses (stETH not available on L2)
        tokens.insert("wsteth".into(), config.tokens.wsteth.parse()?);
        tokens.insert("reth".into(), config.tokens.reth.parse()?);
        tokens.insert("cbeth".into(), config.tokens.cbeth.parse()?);
        tokens.insert("weeth".into(), config.tokens.weeth.parse()?);
        tokens.insert("ezeth".into(), config.tokens.ezeth.parse()?);

        Ok(ParsedConfig {
            // Arbitrum WETH address
            weth: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".parse()?,
            tokens,
            venues: ParsedVenues {
                curve_steth: config.venues.curve_steth_pool.parse()?,
                curve_reth: config.venues.curve_reth_pool.parse()?,
                balancer_vault: config.venues.balancer_vault.parse()?,
                uniswap_quoter: config.venues.uniswap_quoter.parse()?,
                uniswap_router: config.venues.uniswap_router.parse()?,
                multicall3: config.venues.multicall3.parse()?,
            },
            arb_contract: config.execution.arb_contract.parse().unwrap_or(Address::zero()),
            min_spread_bps: config.strategy.min_spread_bps,
            min_profit: U256::from_dec_str(&config.strategy.min_profit_wei)?,
            // max_trade_size removed - determined by convex optimization solver
        })
    }
}

/// Represents a dynamically discovered token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    /// Unique identifier/name for the token
    pub name: String,
    /// Token contract address
    pub address: Address,
    /// Token symbol (e.g., "WETH", "USDC")
    pub symbol: String,
    /// Number of decimals
    pub decimals: u8,
    /// Whether this token has been verified as safe
    pub verified: bool,
}

impl Token {
    /// Create a new token
    pub fn new(name: String, address: Address, symbol: String, decimals: u8) -> Self {
        Self {
            name,
            address,
            symbol,
            decimals,
            verified: false,
        }
    }

    /// Mark token as verified safe
    pub fn mark_verified(&mut self) {
        self.verified = true;
    }
}

/// Runtime configuration that supports dynamic updates via RwLock
///
/// This struct wraps configuration values that can be safely updated
/// at runtime without restarting the bot.
pub struct RuntimeConfig {
    /// Active token list protected by RwLock for concurrent access
    tokens: RwLock<Vec<Token>>,
    /// Parsed static configuration
    pub parsed: ParsedConfig,
}

impl RuntimeConfig {
    /// Create a new RuntimeConfig from a ParsedConfig
    pub fn new(parsed: ParsedConfig) -> Self {
        // Initialize with tokens from parsed config
        let initial_tokens: Vec<Token> = parsed.tokens.iter()
            .map(|(name, address)| Token {
                name: name.clone(),
                address: *address,
                symbol: name.to_uppercase(),
                decimals: 18, // Default for most LSTs
                verified: true, // Pre-configured tokens are assumed verified
            })
            .collect();

        Self {
            tokens: RwLock::new(initial_tokens),
            parsed,
        }
    }

    /// Safely update the active token list at runtime
    ///
    /// This method acquires a write lock and atomically swaps the token list.
    /// Any pending operations using the old list will complete with their
    /// existing references before the new list takes effect.
    ///
    /// # Arguments
    /// * `new_tokens` - The new list of tokens to use
    ///
    /// # Example
    /// ```ignore
    /// let runtime_config = RuntimeConfig::new(parsed_config);
    /// let new_tokens = vec![
    ///     Token::new("weth".into(), weth_addr, "WETH".into(), 18),
    ///     Token::new("usdc".into(), usdc_addr, "USDC".into(), 6),
    /// ];
    /// runtime_config.update_tokens(new_tokens).await;
    /// ```
    pub async fn update_tokens(&self, new_tokens: Vec<Token>) {
        let mut tokens = self.tokens.write().await;
        let old_count = tokens.len();
        let new_count = new_tokens.len();
        *tokens = new_tokens;
        tracing::info!(
            "Token list updated: {} -> {} tokens",
            old_count,
            new_count
        );
    }

    /// Get a read-only snapshot of the current token list
    pub async fn get_tokens(&self) -> Vec<Token> {
        let tokens = self.tokens.read().await;
        tokens.clone()
    }

    /// Get tokens as (Address, String) pairs for compatibility with existing code
    pub async fn get_token_pairs(&self) -> Vec<(Address, String)> {
        let tokens = self.tokens.read().await;
        tokens.iter()
            .filter(|t| t.verified)
            .map(|t| (t.address, t.name.clone()))
            .collect()
    }

    /// Add a single token to the list
    pub async fn add_token(&self, token: Token) {
        let mut tokens = self.tokens.write().await;
        // Check if token already exists
        if !tokens.iter().any(|t| t.address == token.address) {
            tracing::info!(
                "Adding new token: {} ({:?})",
                token.symbol,
                token.address
            );
            tokens.push(token);
        }
    }

    /// Remove a token by address
    pub async fn remove_token(&self, address: Address) {
        let mut tokens = self.tokens.write().await;
        let initial_len = tokens.len();
        tokens.retain(|t| t.address != address);
        if tokens.len() < initial_len {
            tracing::info!("Removed token: {:?}", address);
        }
    }

    /// Check if a token exists in the list
    pub async fn has_token(&self, address: Address) -> bool {
        let tokens = self.tokens.read().await;
        tokens.iter().any(|t| t.address == address)
    }

    /// Get token count
    pub async fn token_count(&self) -> usize {
        let tokens = self.tokens.read().await;
        tokens.len()
    }
}
