use ethers::prelude::*;
use ethers::types::{Address, U256, Bytes, TransactionRequest};
use std::sync::Arc;
use tracing::{debug, warn, info};

use crate::rpc::WsClient;
use crate::detector::Opportunity;
use crate::price::Venue;

/// Expected chain ID for Arbitrum One
const ARBITRUM_CHAIN_ID: u64 = 42161;

abigen!(
    LstArbitrage,
    r#"[
        function executeArb(address lst, uint256 amount, uint8 buyVenue, uint8 sellVenue, uint256 minProfit) external
        function simulateArb(address lst, uint256 amount, uint8 buyVenue, uint8 sellVenue) external returns (uint256 expectedProfit)
    ]"#
);

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub success: bool,
    pub expected_profit: U256,
    pub gas_estimate: U256,
    pub gas_cost_wei: U256,
    pub net_profit: U256,
    pub revert_reason: Option<String>,
}

pub struct Simulator {
    arb_contract: Address,
}

impl Simulator {
    pub fn new(arb_contract: Address) -> Self {
        Self { arb_contract }
    }

    /// Verify we're connected to Arbitrum One (chain ID 42161)
    pub async fn verify_chain(&self, client: Arc<WsClient>) -> eyre::Result<bool> {
        let chain_id = client.get_chainid().await?;
        let chain_id_u64 = chain_id.as_u64();

        if chain_id_u64 != ARBITRUM_CHAIN_ID {
            warn!(
                "Chain ID mismatch! Expected {} (Arbitrum One), got {}",
                ARBITRUM_CHAIN_ID, chain_id_u64
            );
            return Ok(false);
        }

        info!("Connected to Arbitrum One (chain ID: {})", chain_id_u64);
        Ok(true)
    }

    /// Simulate the arbitrage transaction using eth_call
    /// This is the final check before execution
    pub async fn simulate(
        &self,
        client: Arc<WsClient>,
        opportunity: &Opportunity,
        gas_price: U256,
    ) -> eyre::Result<SimulationResult> {
        let contract = LstArbitrage::new(self.arb_contract, client.clone());
        
        // Build the simulation call
        let buy_venue = opportunity.buy_venue.to_u8();
        let sell_venue = opportunity.sell_venue.to_u8();
        
        // First, try to estimate gas
        let call = contract.execute_arb(
            opportunity.token,
            opportunity.trade_amount,
            buy_venue,
            sell_venue,
            U256::zero(), // Set minProfit to 0 for simulation
        );
        
        // Use eth_call to simulate
        match call.call().await {
            Ok(_) => {
                // Estimate gas
                let gas_estimate = match call.estimate_gas().await {
                    Ok(gas) => gas,
                    Err(_) => U256::from(500_000u64), // Default estimate
                };
                
                let gas_cost = gas_estimate * gas_price;
                
                // Calculate expected profit from opportunity data
                let expected_profit = opportunity.expected_profit;
                
                let net_profit = if expected_profit > gas_cost {
                    expected_profit - gas_cost
                } else {
                    U256::zero()
                };
                
                Ok(SimulationResult {
                    success: net_profit > U256::zero(),
                    expected_profit,
                    gas_estimate,
                    gas_cost_wei: gas_cost,
                    net_profit,
                    revert_reason: None,
                })
            }
            Err(e) => {
                // Extract revert reason if available
                let revert_reason = extract_revert_reason(&e);
                
                warn!(
                    "Simulation failed for {}: {:?}",
                    opportunity.token_name,
                    revert_reason
                );
                
                Ok(SimulationResult {
                    success: false,
                    expected_profit: U256::zero(),
                    gas_estimate: U256::zero(),
                    gas_cost_wei: U256::zero(),
                    net_profit: U256::zero(),
                    revert_reason: Some(revert_reason),
                })
            }
        }
    }
    
    /// Quick simulation without full gas estimation
    /// Used for rapid filtering
    pub async fn quick_simulate(
        &self,
        client: Arc<WsClient>,
        opportunity: &Opportunity,
    ) -> bool {
        let contract = LstArbitrage::new(self.arb_contract, client.clone());
        
        let call = contract.execute_arb(
            opportunity.token,
            opportunity.trade_amount,
            opportunity.buy_venue.to_u8(),
            opportunity.sell_venue.to_u8(),
            U256::zero(),
        );
        
        call.call().await.is_ok()
    }
    
    /// Build the actual transaction for execution
    /// Uses the passed client instead of hardcoded RPC URL
    pub fn build_transaction(
        &self,
        opportunity: &Opportunity,
        min_profit: U256,
        gas_limit: U256,
        max_fee_per_gas: U256,
        max_priority_fee: U256,
        nonce: U256,
    ) -> TypedTransaction {
        // Build transaction data manually without needing a provider
        // The executeArb function signature: executeArb(address,uint256,uint8,uint8,uint256)
        let selector = ethers::utils::id("executeArb(address,uint256,uint8,uint8,uint256)");

        let mut data = selector[0..4].to_vec();

        // Encode parameters (each padded to 32 bytes)
        // 1. address lst (padded to 32 bytes)
        data.extend_from_slice(&[0u8; 12]); // 12 zero bytes
        data.extend_from_slice(opportunity.token.as_bytes());

        // 2. uint256 amount
        let mut amount_bytes = [0u8; 32];
        opportunity.trade_amount.to_big_endian(&mut amount_bytes);
        data.extend_from_slice(&amount_bytes);

        // 3. uint8 buyVenue (padded to 32 bytes)
        let mut buy_venue_bytes = [0u8; 32];
        buy_venue_bytes[31] = opportunity.buy_venue.to_u8();
        data.extend_from_slice(&buy_venue_bytes);

        // 4. uint8 sellVenue (padded to 32 bytes)
        let mut sell_venue_bytes = [0u8; 32];
        sell_venue_bytes[31] = opportunity.sell_venue.to_u8();
        data.extend_from_slice(&sell_venue_bytes);

        // 5. uint256 minProfit
        let mut min_profit_bytes = [0u8; 32];
        min_profit.to_big_endian(&mut min_profit_bytes);
        data.extend_from_slice(&min_profit_bytes);

        // Build EIP-1559 transaction for Arbitrum
        let tx = Eip1559TransactionRequest {
            to: Some(self.arb_contract.into()),
            data: Some(data.into()),
            gas: Some(gas_limit),
            max_fee_per_gas: Some(max_fee_per_gas),
            max_priority_fee_per_gas: Some(max_priority_fee),
            nonce: Some(nonce),
            chain_id: Some(ARBITRUM_CHAIN_ID.into()),
            ..Default::default()
        };

        TypedTransaction::Eip1559(tx)
    }
}

fn extract_revert_reason(error: &ContractError<Provider<Ws>>) -> String {
    match error {
        ContractError::Revert(bytes) => {
            // Try to decode as string
            if bytes.len() > 68 {
                // Skip selector (4 bytes) and offset (32 bytes) and length (32 bytes)
                let string_data = &bytes[68..];
                if let Ok(s) = String::from_utf8(string_data.to_vec()) {
                    return s.trim_matches('\0').to_string();
                }
            }
            format!("Revert: 0x{}", hex::encode(bytes))
        }
        _ => format!("{:?}", error),
    }
}
