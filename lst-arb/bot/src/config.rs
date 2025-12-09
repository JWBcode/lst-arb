use ethers::types::{Address, U256};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::str::FromStr;

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
    // LSTs
    pub steth: String,
    pub reth: String,
    pub cbeth: String,
    pub wsteth: String,
    // LRTs
    pub weeth: String,
    pub ezeth: String,
    pub rseth: String,
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
    pub max_trade_size_eth: f64,
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
                primary: std::env::var("RPC_URL_PRIMARY")
                    .unwrap_or_else(|_| "wss://eth-mainnet.g.alchemy.com/v2/demo".into()),
                backup1: std::env::var("RPC_URL_BACKUP1")
                    .unwrap_or_else(|_| "wss://eth-mainnet.g.alchemy.com/v2/demo".into()),
                backup2: std::env::var("RPC_URL_BACKUP2")
                    .unwrap_or_else(|_| "wss://mainnet.infura.io/ws/v3/demo".into()),
                health_check_interval_ms: 5000,
                max_latency_ms: 100,
            },
            tokens: TokenConfig {
                steth: "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84".into(),
                reth: "0xae78736Cd615f374D3085123A210448E74Fc6393".into(),
                cbeth: "0xBe9895146f7AF43049ca1c1AE358B0541Ea49704".into(),
                wsteth: "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0".into(),
                weeth: "0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee".into(),
                ezeth: "0xbf5495Efe5DB9ce00f80364C8B423567e58d2110".into(),
                rseth: "0xA1290d69c65A6Fe4DF752f95823fae25cB99e5A7".into(),
            },
            venues: VenueConfig {
                curve_steth_pool: "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022".into(),
                curve_reth_pool: "0x0f3159811670c117c372428D4E69AC32325e4D0F".into(),
                balancer_vault: "0xBA12222222228d8Ba445958a75a0704d566BF2C8".into(),
                uniswap_quoter: "0x61fFE014bA17989E743c5F6cB21bF9697530B21e".into(),
                uniswap_router: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                multicall3: "0xcA11bde05977b3631167028862bE2a173976CA11".into(),
            },
            strategy: StrategyConfig {
                min_spread_bps: 20,
                min_profit_wei: "10000000000000000".into(), // 0.01 ETH
                max_trade_size_eth: 10.0,
                poll_interval_ms: 200,
                enabled_tokens: vec![
                    "steth".into(),
                    "reth".into(), 
                    "weeth".into(),
                    "ezeth".into(),
                ],
            },
            execution: ExecutionConfig {
                use_flashbots: true,
                flashbots_relay: "https://relay.flashbots.net".into(),
                max_gas_price_gwei: 100,
                max_priority_fee_gwei: 50,
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
    pub max_trade_size: U256,
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
        tokens.insert("steth".into(), config.tokens.steth.parse()?);
        tokens.insert("reth".into(), config.tokens.reth.parse()?);
        tokens.insert("cbeth".into(), config.tokens.cbeth.parse()?);
        tokens.insert("wsteth".into(), config.tokens.wsteth.parse()?);
        tokens.insert("weeth".into(), config.tokens.weeth.parse()?);
        tokens.insert("ezeth".into(), config.tokens.ezeth.parse()?);
        tokens.insert("rseth".into(), config.tokens.rseth.parse()?);
        
        Ok(ParsedConfig {
            weth: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse()?,
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
            max_trade_size: ethers::utils::parse_ether(config.strategy.max_trade_size_eth)?,
        })
    }
}
