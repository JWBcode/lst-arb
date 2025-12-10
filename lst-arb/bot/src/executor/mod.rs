use ethers::prelude::*;
use ethers::types::{Address, U256, Bytes, TransactionRequest, H256};
use ethers::signers::LocalWallet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn, error};

use crate::rpc::WsClient;
use crate::detector::Opportunity;
use crate::simulator::{Simulator, SimulationResult};

pub struct Executor {
    wallet: LocalWallet,
    arb_contract: Address,
    simulator: Simulator,
    nonce: AtomicU64,
    use_flashbots: bool,
    flashbots_relay: String,
    pending_txs: RwLock<Vec<PendingTx>>,
    max_gas_price: U256,
    max_priority_fee: U256,
}

#[derive(Debug, Clone)]
pub struct PendingTx {
    pub hash: H256,
    pub opportunity: Opportunity,
    pub submitted_at: std::time::Instant,
    pub gas_price: U256,
}

#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Submitted { hash: H256 },
    Confirmed { hash: H256, profit: U256 },
    Reverted { hash: H256, reason: String },
    Failed { reason: String },
    /// Transaction aborted due to pre-flight check failure
    /// (price moved too much during the detection-to-execution window)
    Aborted { expected_profit: U256, actual_profit: U256 },
}

/// Result of pre-flight execution integrity verification
#[derive(Debug, Clone)]
enum VerificationResult {
    /// Verification passed, profit is acceptable
    Passed { actual_profit: U256 },
    /// Profit degraded by more than 10%
    ProfitDegraded { expected: U256, actual: U256 },
    /// Simulation reverted (conditions changed)
    SimulationReverted { reason: String },
}

impl Executor {
    pub async fn new(
        client: Arc<WsClient>,
        wallet: LocalWallet,
        arb_contract: Address,
        use_flashbots: bool,
        flashbots_relay: String,
        max_gas_price_gwei: u64,
        max_priority_fee_gwei: u64,
    ) -> eyre::Result<Self> {
        // Fetch initial nonce
        let nonce = client.get_transaction_count(wallet.address(), None).await?;
        
        Ok(Self {
            wallet,
            arb_contract,
            simulator: Simulator::new(arb_contract),
            nonce: AtomicU64::new(nonce.as_u64()),
            use_flashbots,
            flashbots_relay,
            pending_txs: RwLock::new(Vec::new()),
            max_gas_price: ethers::utils::parse_units(max_gas_price_gwei, "gwei")?.into(),
            max_priority_fee: ethers::utils::parse_units(max_priority_fee_gwei, "gwei")?.into(),
        })
    }
    
    /// Execute an arbitrage opportunity
    /// Optimized for Arbitrum's FIFO sequencer (no MEV, no priority fees)
    pub async fn execute(
        &self,
        client: Arc<WsClient>,
        opportunity: &Opportunity,
    ) -> eyre::Result<ExecutionResult> {
        // Step 1: Get current gas price
        // On Arbitrum, the RPC estimate includes L1 data fee, so we trust it
        let gas_price = client.get_gas_price().await?;

        // On Arbitrum L2, gas prices are typically very low (0.1 gwei)
        // No need to add priority fee - sequencer uses FIFO ordering
        if gas_price > self.max_gas_price {
            return Ok(ExecutionResult::Failed {
                reason: format!("Gas price too high: {} > {}", gas_price, self.max_gas_price),
            });
        }

        // Step 2: Simulate
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

        // Step 3: Check profitability after gas
        if sim_result.net_profit.is_zero() {
            return Ok(ExecutionResult::Failed {
                reason: "Not profitable after gas".into(),
            });
        }

        // Step 4: Build transaction
        let nonce = self.get_and_increment_nonce();

        // Set minProfit to 80% of expected to account for slippage
        let min_profit = sim_result.net_profit * 80 / 100;

        let gas_limit = sim_result.gas_estimate * 120 / 100; // 20% buffer

        // On Arbitrum, no priority fee needed (FIFO sequencer)
        let priority_fee = U256::zero();

        let tx = self.simulator.build_transaction(
            opportunity,
            min_profit,
            gas_limit,
            gas_price,
            priority_fee,
            U256::from(nonce),
        );

        // Step 5: Pre-flight verification
        // Verify execution integrity IMMEDIATELY before submission
        // This catches price movements during the ~50ms detection-to-execution window
        match self.verify_execution_integrity(
            client.clone(),
            opportunity,
            sim_result.net_profit,
        ).await {
            Ok(VerificationResult::Passed { actual_profit }) => {
                info!(
                    "✅ Pre-flight check passed. Expected: {} ETH, Actual: {} ETH",
                    ethers::utils::format_ether(sim_result.net_profit),
                    ethers::utils::format_ether(actual_profit)
                );
            }
            Ok(VerificationResult::ProfitDegraded { expected, actual }) => {
                warn!(
                    "⚠️ Pre-flight check failed. Expected {} ETH, got {} ETH. Saving gas.",
                    ethers::utils::format_ether(expected),
                    ethers::utils::format_ether(actual)
                );
                return Ok(ExecutionResult::Aborted {
                    expected_profit: expected,
                    actual_profit: actual,
                });
            }
            Ok(VerificationResult::SimulationReverted { reason }) => {
                warn!(
                    "⚠️ Pre-flight check failed. Simulation reverted: {}. Saving gas.",
                    reason
                );
                return Ok(ExecutionResult::Aborted {
                    expected_profit: sim_result.net_profit,
                    actual_profit: U256::zero(),
                });
            }
            Err(e) => {
                // On verification error, be conservative and abort
                warn!("⚠️ Pre-flight verification error: {:?}. Aborting.", e);
                return Ok(ExecutionResult::Aborted {
                    expected_profit: sim_result.net_profit,
                    actual_profit: U256::zero(),
                });
            }
        }

        // Step 6: Sign transaction
        let signature = self.wallet.sign_transaction(&tx).await?;
        let signed_tx = tx.rlp_signed(&signature);

        // Step 7: Submit directly (Flashbots not available on Arbitrum)
        // Arbitrum uses FIFO ordering, so direct submission is optimal
        self.submit_direct(client.clone(), &signed_tx, opportunity).await
    }

    /// Verify execution integrity immediately before submission
    ///
    /// Performs a fresh simulation against the latest block to catch price movements
    /// during the detection-to-execution window (~50ms on Arbitrum).
    ///
    /// Aborts if:
    /// - Actual profit < 90% of expected profit (price moved >10%)
    /// - Simulation reverts (conditions changed)
    async fn verify_execution_integrity(
        &self,
        client: Arc<WsClient>,
        opportunity: &Opportunity,
        expected_profit: U256,
    ) -> eyre::Result<VerificationResult> {
        // Simulate against the latest block
        let gas_price = client.get_gas_price().await?;

        let sim_result = self.simulator.simulate(
            client.clone(),
            opportunity,
            gas_price,
        ).await?;

        // Check if simulation reverted
        if !sim_result.success {
            return Ok(VerificationResult::SimulationReverted {
                reason: sim_result.revert_reason.unwrap_or_else(|| "Unknown".to_string()),
            });
        }

        let actual_profit = sim_result.net_profit;

        // Check if profit degraded by more than 10%
        // Formula: actual_profit < expected_profit * 0.90
        let min_acceptable_profit = expected_profit * 90 / 100;

        if actual_profit < min_acceptable_profit {
            return Ok(VerificationResult::ProfitDegraded {
                expected: expected_profit,
                actual: actual_profit,
            });
        }

        Ok(VerificationResult::Passed { actual_profit })
    }

    /// Submit transaction directly to Arbitrum sequencer
    /// Optimized for FIFO ordering - no priority fee bumping needed
    async fn submit_direct(
        &self,
        client: Arc<WsClient>,
        signed_tx: &Bytes,
        opportunity: &Opportunity,
    ) -> eyre::Result<ExecutionResult> {
        // On Arbitrum, transactions are processed in FIFO order by the sequencer
        // No need for priority fee optimization or replacement strategies
        let pending = client.send_raw_transaction(signed_tx.clone()).await?;
        let hash = pending.tx_hash();

        info!("📤 TX submitted to Arbitrum sequencer: {:?}", hash);

        // Track pending transaction
        {
            let mut pending_txs = self.pending_txs.write().await;
            pending_txs.push(PendingTx {
                hash,
                opportunity: opportunity.clone(),
                submitted_at: std::time::Instant::now(),
                gas_price: U256::zero(),
            });
        }

        Ok(ExecutionResult::Submitted { hash })
    }
    
    async fn submit_flashbots(
        &self,
        client: Arc<WsClient>,
        signed_tx: &Bytes,
        opportunity: &Opportunity,
    ) -> eyre::Result<ExecutionResult> {
        // For Flashbots Protect, we submit to their RPC endpoint
        // This provides frontrunning protection without needing bundles
        
        let flashbots_client = reqwest::Client::new();
        
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_sendRawTransaction",
            "params": [format!("0x{}", hex::encode(signed_tx))],
            "id": 1
        });
        
        let response = flashbots_client
            .post(&self.flashbots_relay)
            .json(&request)
            .send()
            .await?;
        
        let result: serde_json::Value = response.json().await?;
        
        if let Some(hash) = result.get("result").and_then(|v| v.as_str()) {
            let hash: H256 = hash.parse()?;
            
            info!("📤 TX submitted via Flashbots: {:?}", hash);
            
            // Track pending transaction
            {
                let mut pending_txs = self.pending_txs.write().await;
                pending_txs.push(PendingTx {
                    hash,
                    opportunity: opportunity.clone(),
                    submitted_at: std::time::Instant::now(),
                    gas_price: U256::zero(),
                });
            }
            
            Ok(ExecutionResult::Submitted { hash })
        } else if let Some(error) = result.get("error") {
            let reason = error.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            
            warn!("Flashbots submission failed: {}", reason);
            
            Ok(ExecutionResult::Failed { reason })
        } else {
            Ok(ExecutionResult::Failed {
                reason: "Unknown Flashbots response".into(),
            })
        }
    }
    
    /// Check status of pending transactions
    pub async fn check_pending(&self, client: Arc<WsClient>) -> Vec<ExecutionResult> {
        let mut results = Vec::new();
        let mut completed_hashes = Vec::new();
        
        let pending_txs = self.pending_txs.read().await;
        
        for pending in pending_txs.iter() {
            if let Ok(Some(receipt)) = client.get_transaction_receipt(pending.hash).await {
                completed_hashes.push(pending.hash);
                
                if receipt.status == Some(1.into()) {
                    // Success! Calculate actual profit from logs if available
                    info!("✅ TX confirmed: {:?}", pending.hash);
                    results.push(ExecutionResult::Confirmed {
                        hash: pending.hash,
                        profit: pending.opportunity.expected_profit, // Could parse from logs
                    });
                } else {
                    warn!("❌ TX reverted: {:?}", pending.hash);
                    results.push(ExecutionResult::Reverted {
                        hash: pending.hash,
                        reason: "Transaction reverted".into(),
                    });
                }
            } else if pending.submitted_at.elapsed() > std::time::Duration::from_secs(120) {
                // TX stuck for >2 minutes
                warn!("⏰ TX stuck: {:?}", pending.hash);
                completed_hashes.push(pending.hash);
            }
        }
        
        // Remove completed transactions
        drop(pending_txs);
        {
            let mut pending_txs = self.pending_txs.write().await;
            pending_txs.retain(|tx| !completed_hashes.contains(&tx.hash));
        }
        
        results
    }
    
    fn get_and_increment_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }
    
    /// Reset nonce from chain (call after failed tx)
    pub async fn resync_nonce(&self, client: Arc<WsClient>) -> eyre::Result<()> {
        let nonce = client.get_transaction_count(self.wallet.address(), None).await?;
        self.nonce.store(nonce.as_u64(), Ordering::SeqCst);
        Ok(())
    }
}
