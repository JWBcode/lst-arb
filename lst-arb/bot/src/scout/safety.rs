//! Honey Pot Detection and Token Safety Checks
//!
//! Implements Active Defense mechanisms to prevent the bot from:
//! - Trading scam/tax tokens that trap funds
//! - Interacting with paused/broken tokens
//! - Wasting gas on malicious contracts

use ethers::prelude::*;
use ethers::types::{Address, Bytes, TransactionRequest, U256};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::rpc::WsClient;

/// Maximum gas for a simple ERC20 transfer (anything higher indicates tax/scam token)
const MAX_TRANSFER_GAS: u64 = 100_000;

/// Minimum gas for simulation (standard ERC20 transfer ~21k + ~30k for token logic)
const MIN_SIMULATION_GAS: u64 = 150_000;

abigen!(
    IERC20Safety,
    r#"[
        function transfer(address to, uint256 amount) external returns (bool)
        function balanceOf(address account) external view returns (uint256)
    ]"#
);

/// Token safety checker for honey pot detection
pub struct SafetyChecker {
    /// Bot's own address for self-transfer simulation
    bot_address: Address,
}

impl SafetyChecker {
    pub fn new(bot_address: Address) -> Self {
        Self { bot_address }
    }

    /// Check if a token is safe to trade
    ///
    /// Performs a simulated eth_call transfer of 1 wei from the bot to itself.
    /// This detects:
    /// - Paused/broken tokens (call reverts)
    /// - Tax/scam tokens (excessive gas usage > 100k)
    /// - Blacklisted addresses
    /// - Transfer restrictions
    ///
    /// Returns true if token is safe, false otherwise
    pub async fn check_token_safety(
        &self,
        token_address: Address,
        client: Arc<WsClient>,
    ) -> bool {
        // Build a simulated transfer of 1 wei from bot to itself
        let transfer_calldata = self.encode_transfer_call(self.bot_address, U256::one());

        let tx = TransactionRequest::new()
            .from(self.bot_address)
            .to(token_address)
            .data(transfer_calldata)
            .gas(MIN_SIMULATION_GAS);

        // Perform eth_call simulation
        match client.estimate_gas(&tx.clone().into(), None).await {
            Ok(gas_estimate) => {
                let gas_used = gas_estimate.as_u64();

                if gas_used > MAX_TRANSFER_GAS {
                    warn!(
                        "Token {:?} failed safety check: gas too high ({} > {}). Likely tax/scam token.",
                        token_address, gas_used, MAX_TRANSFER_GAS
                    );
                    return false;
                }

                debug!(
                    "Token {:?} passed safety check: gas estimate = {}",
                    token_address, gas_used
                );
                true
            }
            Err(e) => {
                // Check if it's a revert
                let error_msg = e.to_string().to_lowercase();

                if error_msg.contains("revert")
                    || error_msg.contains("execution reverted")
                    || error_msg.contains("insufficient")
                    || error_msg.contains("paused")
                    || error_msg.contains("blacklist")
                {
                    warn!(
                        "Token {:?} failed safety check: transfer reverted. Error: {}",
                        token_address, e
                    );
                    return false;
                }

                // For other errors (network issues), be conservative and fail
                warn!(
                    "Token {:?} safety check error (failing safe): {}",
                    token_address, e
                );
                false
            }
        }
    }

    /// Alternative check using eth_call directly for more detailed error info
    pub async fn check_token_safety_detailed(
        &self,
        token_address: Address,
        client: Arc<WsClient>,
    ) -> TokenSafetyResult {
        let transfer_calldata = self.encode_transfer_call(self.bot_address, U256::one());

        let tx = TransactionRequest::new()
            .from(self.bot_address)
            .to(token_address)
            .data(transfer_calldata)
            .gas(MIN_SIMULATION_GAS);

        // First, try eth_call to see if it reverts
        match client.call(&tx.clone().into(), None).await {
            Ok(_) => {
                // Call succeeded, now check gas usage
                match client.estimate_gas(&tx.into(), None).await {
                    Ok(gas_estimate) => {
                        let gas_used = gas_estimate.as_u64();

                        if gas_used > MAX_TRANSFER_GAS {
                            TokenSafetyResult::TaxToken { gas_used }
                        } else {
                            TokenSafetyResult::Safe { gas_used }
                        }
                    }
                    Err(e) => TokenSafetyResult::Error {
                        reason: e.to_string(),
                    },
                }
            }
            Err(e) => {
                let error_msg = e.to_string().to_lowercase();

                if error_msg.contains("paused") {
                    TokenSafetyResult::Paused
                } else if error_msg.contains("blacklist") {
                    TokenSafetyResult::Blacklisted
                } else {
                    TokenSafetyResult::Reverted {
                        reason: e.to_string(),
                    }
                }
            }
        }
    }

    /// Encode ERC20 transfer function call
    fn encode_transfer_call(&self, to: Address, amount: U256) -> Bytes {
        // transfer(address,uint256) selector = 0xa9059cbb
        let selector = [0xa9, 0x05, 0x9c, 0xbb];

        let mut data = Vec::with_capacity(68);
        data.extend_from_slice(&selector);

        // Pad address to 32 bytes
        let mut to_padded = [0u8; 32];
        to_padded[12..].copy_from_slice(to.as_bytes());
        data.extend_from_slice(&to_padded);

        // Encode amount as 32 bytes
        let mut amount_bytes = [0u8; 32];
        amount.to_big_endian(&mut amount_bytes);
        data.extend_from_slice(&amount_bytes);

        Bytes::from(data)
    }
}

/// Detailed result of token safety check
#[derive(Debug, Clone)]
pub enum TokenSafetyResult {
    /// Token is safe to trade
    Safe { gas_used: u64 },
    /// Token has excessive gas usage (tax/fee on transfer)
    TaxToken { gas_used: u64 },
    /// Token is paused
    Paused,
    /// Address is blacklisted
    Blacklisted,
    /// Transfer reverted for unknown reason
    Reverted { reason: String },
    /// Error during check
    Error { reason: String },
}

impl TokenSafetyResult {
    pub fn is_safe(&self) -> bool {
        matches!(self, TokenSafetyResult::Safe { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_transfer_call() {
        let checker = SafetyChecker::new(Address::zero());
        let to = "0x1234567890123456789012345678901234567890"
            .parse::<Address>()
            .unwrap();
        let amount = U256::from(1000);

        let calldata = checker.encode_transfer_call(to, amount);

        // Check selector
        assert_eq!(&calldata[0..4], &[0xa9, 0x05, 0x9c, 0xbb]);

        // Check that calldata is correct length (4 + 32 + 32 = 68)
        assert_eq!(calldata.len(), 68);
    }

    #[test]
    fn test_safety_result_is_safe() {
        assert!(TokenSafetyResult::Safe { gas_used: 50000 }.is_safe());
        assert!(!TokenSafetyResult::TaxToken { gas_used: 150000 }.is_safe());
        assert!(!TokenSafetyResult::Paused.is_safe());
        assert!(!TokenSafetyResult::Blacklisted.is_safe());
    }
}
