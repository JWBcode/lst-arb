//! Arbitrum-Optimized Transaction Executor
//!
//! Optimized for Arbitrum's FIFO sequencer:
//! - No Flashbots (Arbitrum has no public mempool)
//! - Direct submission via send_raw_transaction
//! - Aggressive re-submission for dropped transactions
//! - Gas optimization with 20% buffer

use ethers::prelude::*;
use ethers::types::{Address, U256, Bytes, H256};
use ethers::signers::LocalWallet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn, debug};

use crate::rpc::WsClient;
use crate::detector::Opportunity;
use crate::simulator::Simulator;

/// Arbitrum block time is ~250ms
const ARBITRUM_BLOCK_TIME_MS: u64 = 250;

/// Wait time before re-submission (2 blocks)
const RESUBMIT_WAIT_MS: u64 = 500;

/// Maximum re-submission attempts
const MAX_RESUBMIT_ATTEMPTS: u32 = 3;

/// Gas estimation buffer (20%)
const GAS_BUFFER_PERCENT: u64 = 120;

/// Stuck transaction timeout (2 minutes)
const STUCK_TX_TIMEOUT_SECS: u64 = 120;

pub struct Executor {
    wallet: LocalWallet,
    arb_contract: Address,
    simulator: Simulator,
    nonce: AtomicU64,
    pending_txs: RwLock<Vec<PendingTx>>,
    max_gas_price: U256,
}

#[derive(Debug, Clone)]
pub struct PendingTx {
    pub hash: H256,
    pub opportunity: Opportunity,
    pub submitted_at: Instant,
    pub gas_price: U256,
    pub resubmit_count: u32,
}

#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Submitted { hash: H256 },
    Confirmed { hash: H256, profit: U256 },
    Reverted { hash: H256, reason: String },
    Failed { reason: String },
}

impl Executor {
    /// Create a new executor optimized for Arbitrum
    ///
    /// Note: `use_flashbots` and `flashbots_relay` parameters are ignored
    /// as Arbitrum uses a FIFO sequencer with no public mempool.
    pub async fn new(
        client: Arc<WsClient>,
        wallet: LocalWallet,
        arb_contract: Address,
        _use_flashbots: bool,      // Ignored - Arbitrum has no Flashbots
        _flashbots_relay: String,  // Ignored - Arbitrum has no Flashbots
        max_gas_price_gwei: u64,
        _max_priority_fee_gwei: u64, // Ignored - Arbitrum uses FIFO, no priority fee needed
    ) -> eyre::Result<Self> {
        // Fetch initial nonce
        let nonce = client.get_transaction_count(wallet.address(), None).await?;

        info!("Executor initialized for Arbitrum (FIFO sequencer mode)");
        info!("  Max gas price: {} gwei", max_gas_price_gwei);
        info!("  Re-submission: {} attempts with {}ms wait", MAX_RESUBMIT_ATTEMPTS, RESUBMIT_WAIT_MS);

        Ok(Self {
            wallet,
            arb_contract,
            simulator: Simulator::new(arb_contract),
            nonce: AtomicU64::new(nonce.as_u64()),
            pending_txs: RwLock::new(Vec::new()),
            max_gas_price: ethers::utils::parse_units(max_gas_price_gwei, "gwei")?.into(),
        })
    }

    /// Execute an arbitrage opportunity on Arbitrum
    ///
    /// Uses direct submission with aggressive re-submission logic:
    /// 1. Submit transaction
    /// 2. Wait 500ms for receipt
    /// 3. If still pending, re-submit same tx (same nonce) up to 3 times
    pub async fn execute(
        &self,
        client: Arc<WsClient>,
        opportunity: &Opportunity,
    ) -> eyre::Result<ExecutionResult> {
        // Step 1: Get current gas price from Arbitrum sequencer
        // On Arbitrum, this includes the L1 data fee component
        let gas_price = client.get_gas_price().await?;

        // Arbitrum L2 gas is typically very low (0.1 gwei)
        // No priority fee needed - sequencer uses FIFO ordering
        if gas_price > self.max_gas_price {
            return Ok(ExecutionResult::Failed {
                reason: format!(
                    "Gas price too high: {} gwei > {} gwei max",
                    ethers::utils::format_units(gas_price, "gwei").unwrap_or_default(),
                    ethers::utils::format_units(self.max_gas_price, "gwei").unwrap_or_default()
                ),
            });
        }

        // Step 2: Simulate the transaction
        let sim_result = self.simulator.simulate(
            client.clone(),
            opportunity,
            gas_price,
        ).await?;

        if !sim_result.success {
            return Ok(ExecutionResult::Failed {
                reason: sim_result.revert_reason.unwrap_or_else(|| "Simulation failed".into()),
            });
        }

        // Step 3: Check profitability after gas costs
        if sim_result.net_profit.is_zero() {
            return Ok(ExecutionResult::Failed {
                reason: "Not profitable after gas costs".into(),
            });
        }

        // Step 4: Build and sign transaction
        let nonce = self.get_and_increment_nonce();

        // Set minProfit to 80% of expected to account for slippage
        let min_profit = sim_result.net_profit * U256::from(80u64) / U256::from(100u64);

        // Add 20% gas buffer - Arbitrum estimation is reliable but we add safety margin
        let gas_limit = sim_result.gas_estimate * U256::from(GAS_BUFFER_PERCENT) / U256::from(100u64);

        // No priority fee on Arbitrum (FIFO sequencer)
        let priority_fee = U256::zero();

        let tx = self.simulator.build_transaction(
            opportunity,
            min_profit,
            gas_limit,
            gas_price,
            priority_fee,
            U256::from(nonce),
        );

        // Step 5: Sign transaction
        let signature = self.wallet.sign_transaction(&tx).await?;
        let signed_tx = tx.rlp_signed(&signature);

        // Step 6: Submit with aggressive re-submission
        self.submit_with_resubmission(client, &signed_tx, opportunity, gas_price, nonce).await
    }

    /// Submit transaction with aggressive re-submission logic
    ///
    /// Arbitrum blocks are ~250ms. If we don't see a receipt within 500ms,
    /// the transaction might have been dropped. Re-submit the exact same
    /// transaction (same nonce) to ensure propagation to sequencer.
    async fn submit_with_resubmission(
        &self,
        client: Arc<WsClient>,
        signed_tx: &Bytes,
        opportunity: &Opportunity,
        gas_price: U256,
        nonce: u64,
    ) -> eyre::Result<ExecutionResult> {
        let mut last_hash: Option<H256> = None;
        let mut attempt = 0;

        loop {
            attempt += 1;

            // Submit/Re-submit the transaction
            match client.send_raw_transaction(signed_tx.clone()).await {
                Ok(pending) => {
                    let hash = pending.tx_hash();
                    last_hash = Some(hash);

                    if attempt == 1 {
                        info!("📤 TX submitted to Arbitrum sequencer: {:?} (nonce: {})", hash, nonce);
                    } else {
                        info!("🔄 TX re-submitted (attempt {}/{}): {:?}", attempt, MAX_RESUBMIT_ATTEMPTS, hash);
                    }

                    // Wait for potential inclusion
                    tokio::time::sleep(Duration::from_millis(RESUBMIT_WAIT_MS)).await;

                    // Check if transaction was included
                    match client.get_transaction_receipt(hash).await {
                        Ok(Some(receipt)) => {
                            // Transaction confirmed!
                            if receipt.status == Some(U64::from(1)) {
                                info!("✅ TX confirmed on attempt {}: {:?}", attempt, hash);
                                return Ok(ExecutionResult::Confirmed {
                                    hash,
                                    profit: opportunity.expected_profit,
                                });
                            } else {
                                warn!("❌ TX reverted on attempt {}: {:?}", attempt, hash);
                                return Ok(ExecutionResult::Reverted {
                                    hash,
                                    reason: "Transaction reverted on-chain".into(),
                                });
                            }
                        }
                        Ok(None) => {
                            // Still pending - will retry if attempts remain
                            debug!("TX still pending after {}ms: {:?}", RESUBMIT_WAIT_MS, hash);
                        }
                        Err(e) => {
                            warn!("Error checking receipt: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("{:?}", e);

                    // Check for known non-retryable errors
                    if error_msg.contains("nonce too low") {
                        warn!("Nonce too low - transaction already included or replaced");
                        // Try to find the actual transaction
                        if let Some(hash) = last_hash {
                            return Ok(ExecutionResult::Submitted { hash });
                        }
                        return Ok(ExecutionResult::Failed {
                            reason: "Nonce too low - transaction may have been included".into(),
                        });
                    }

                    if error_msg.contains("replacement transaction underpriced") {
                        // Transaction with same nonce already in mempool
                        debug!("Transaction already in sequencer queue");
                        if let Some(hash) = last_hash {
                            // Track and return
                            self.track_pending(hash, opportunity, gas_price).await;
                            return Ok(ExecutionResult::Submitted { hash });
                        }
                    }

                    if error_msg.contains("insufficient funds") {
                        return Ok(ExecutionResult::Failed {
                            reason: "Insufficient funds for transaction".into(),
                        });
                    }

                    warn!("Submission error on attempt {}: {}", attempt, error_msg);
                }
            }

            // Check if we should retry
            if attempt >= MAX_RESUBMIT_ATTEMPTS {
                break;
            }

            debug!("Retrying submission (attempt {}/{})", attempt + 1, MAX_RESUBMIT_ATTEMPTS);
        }

        // After all attempts, track the transaction if we have a hash
        if let Some(hash) = last_hash {
            self.track_pending(hash, opportunity, gas_price).await;
            Ok(ExecutionResult::Submitted { hash })
        } else {
            // Decrement nonce since transaction was never submitted
            self.nonce.fetch_sub(1, Ordering::SeqCst);
            Ok(ExecutionResult::Failed {
                reason: format!("Failed to submit after {} attempts", MAX_RESUBMIT_ATTEMPTS),
            })
        }
    }

    /// Track a pending transaction for later status checks
    async fn track_pending(&self, hash: H256, opportunity: &Opportunity, gas_price: U256) {
        let mut pending_txs = self.pending_txs.write().await;
        pending_txs.push(PendingTx {
            hash,
            opportunity: opportunity.clone(),
            submitted_at: Instant::now(),
            gas_price,
            resubmit_count: 0,
        });
    }

    /// Check status of pending transactions
    ///
    /// Called periodically to update status of submitted transactions.
    /// On Arbitrum, transactions should confirm within a few blocks (~1 second).
    pub async fn check_pending(&self, client: Arc<WsClient>) -> Vec<ExecutionResult> {
        let mut results = Vec::new();
        let mut completed_hashes = Vec::new();

        let pending_txs = self.pending_txs.read().await;

        for pending in pending_txs.iter() {
            match client.get_transaction_receipt(pending.hash).await {
                Ok(Some(receipt)) => {
                    completed_hashes.push(pending.hash);

                    if receipt.status == Some(U64::from(1)) {
                        info!("✅ TX confirmed: {:?}", pending.hash);
                        results.push(ExecutionResult::Confirmed {
                            hash: pending.hash,
                            profit: pending.opportunity.expected_profit,
                        });
                    } else {
                        warn!("❌ TX reverted: {:?}", pending.hash);
                        results.push(ExecutionResult::Reverted {
                            hash: pending.hash,
                            reason: "Transaction reverted on-chain".into(),
                        });
                    }
                }
                Ok(None) => {
                    // Still pending
                    let elapsed = pending.submitted_at.elapsed();

                    if elapsed > Duration::from_secs(STUCK_TX_TIMEOUT_SECS) {
                        // Transaction stuck for too long
                        warn!(
                            "⏰ TX stuck for {:?}: {:?}",
                            elapsed,
                            pending.hash
                        );
                        completed_hashes.push(pending.hash);
                        results.push(ExecutionResult::Failed {
                            reason: format!("Transaction stuck for {:?}", elapsed),
                        });
                    } else if elapsed > Duration::from_secs(30) {
                        // Warn about slow confirmation
                        debug!(
                            "TX pending for {:?}: {:?}",
                            elapsed,
                            pending.hash
                        );
                    }
                }
                Err(e) => {
                    warn!("Error checking TX {:?}: {:?}", pending.hash, e);
                }
            }
        }

        // Remove completed transactions
        drop(pending_txs);
        if !completed_hashes.is_empty() {
            let mut pending_txs = self.pending_txs.write().await;
            pending_txs.retain(|tx| !completed_hashes.contains(&tx.hash));
        }

        results
    }

    /// Get and increment the nonce atomically
    fn get_and_increment_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// Reset nonce from chain state
    ///
    /// Call this after failed transactions to resync with on-chain state.
    pub async fn resync_nonce(&self, client: Arc<WsClient>) -> eyre::Result<()> {
        let nonce = client.get_transaction_count(self.wallet.address(), None).await?;
        let old_nonce = self.nonce.swap(nonce.as_u64(), Ordering::SeqCst);
        info!("Nonce resynced: {} -> {}", old_nonce, nonce);
        Ok(())
    }

    /// Get current pending transaction count
    pub async fn pending_count(&self) -> usize {
        self.pending_txs.read().await.len()
    }

    /// Get wallet address
    pub fn address(&self) -> Address {
        self.wallet.address()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gas_buffer_calculation() {
        let estimate = U256::from(100_000u64);
        let buffered = estimate * U256::from(GAS_BUFFER_PERCENT) / U256::from(100u64);
        assert_eq!(buffered, U256::from(120_000u64));
    }

    #[test]
    fn test_min_profit_calculation() {
        let expected = U256::from(1000u64);
        let min = expected * U256::from(80u64) / U256::from(100u64);
        assert_eq!(min, U256::from(800u64));
    }
}
