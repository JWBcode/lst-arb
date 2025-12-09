use ethers::types::{U256, H256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};

use crate::detector::Opportunity;
use crate::executor::ExecutionResult;

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub opportunities_found: u64,
    pub simulations_passed: u64,
    pub txs_submitted: u64,
    pub txs_confirmed: u64,
    pub txs_reverted: u64,
    pub total_profit_wei: U256,
    pub total_gas_spent_wei: U256,
    pub start_time: Option<std::time::Instant>,
}

pub struct Monitor {
    stats: RwLock<Stats>,
    telegram_bot_token: Option<String>,
    telegram_chat_id: Option<String>,
    http_client: reqwest::Client,
}

impl Monitor {
    pub fn new(telegram_bot_token: Option<String>, telegram_chat_id: Option<String>) -> Self {
        Self {
            stats: RwLock::new(Stats {
                start_time: Some(std::time::Instant::now()),
                ..Default::default()
            }),
            telegram_bot_token,
            telegram_chat_id,
            http_client: reqwest::Client::new(),
        }
    }
    
    pub async fn record_opportunity(&self, opportunity: &Opportunity) {
        let mut stats = self.stats.write().await;
        stats.opportunities_found += 1;
        
        // Log opportunity
        info!(
            "📊 Opportunity #{}: {} | Spread: {}bps | Expected: {} ETH",
            stats.opportunities_found,
            opportunity.token_name,
            opportunity.spread_bps,
            ethers::utils::format_ether(opportunity.expected_profit)
        );
    }
    
    pub async fn record_simulation_passed(&self) {
        let mut stats = self.stats.write().await;
        stats.simulations_passed += 1;
    }
    
    pub async fn record_execution(&self, result: &ExecutionResult) {
        let mut stats = self.stats.write().await;
        
        match result {
            ExecutionResult::Submitted { hash } => {
                stats.txs_submitted += 1;
                info!("📤 TX #{} submitted: {:?}", stats.txs_submitted, hash);
            }
            ExecutionResult::Confirmed { hash, profit } => {
                stats.txs_confirmed += 1;
                stats.total_profit_wei += *profit;
                
                let msg = format!(
                    "✅ TX CONFIRMED\nHash: {:?}\nProfit: {} ETH\nTotal P&L: {} ETH",
                    hash,
                    ethers::utils::format_ether(*profit),
                    ethers::utils::format_ether(stats.total_profit_wei)
                );
                
                info!("{}", msg);
                drop(stats); // Release lock before async call
                self.send_telegram(&msg).await;
            }
            ExecutionResult::Reverted { hash, reason } => {
                let mut stats = self.stats.write().await;
                stats.txs_reverted += 1;
                
                let msg = format!(
                    "❌ TX REVERTED\nHash: {:?}\nReason: {}",
                    hash, reason
                );
                
                warn!("{}", msg);
                drop(stats);
                self.send_telegram(&msg).await;
            }
            ExecutionResult::Failed { reason } => {
                warn!("TX Failed: {}", reason);
            }
        }
    }
    
    pub async fn record_gas_spent(&self, gas_cost: U256) {
        let mut stats = self.stats.write().await;
        stats.total_gas_spent_wei += gas_cost;
    }
    
    pub async fn get_stats(&self) -> Stats {
        self.stats.read().await.clone()
    }
    
    pub async fn log_summary(&self) {
        let stats = self.stats.read().await;
        
        let uptime = stats.start_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        
        let hours = uptime / 3600;
        let minutes = (uptime % 3600) / 60;
        
        let net_profit = if stats.total_profit_wei > stats.total_gas_spent_wei {
            stats.total_profit_wei - stats.total_gas_spent_wei
        } else {
            U256::zero()
        };
        
        let win_rate = if stats.txs_submitted > 0 {
            (stats.txs_confirmed as f64 / stats.txs_submitted as f64) * 100.0
        } else {
            0.0
        };
        
        info!("═══════════════════════════════════════════");
        info!("📊 BOT STATISTICS");
        info!("═══════════════════════════════════════════");
        info!("Uptime:              {}h {}m", hours, minutes);
        info!("Opportunities Found: {}", stats.opportunities_found);
        info!("Simulations Passed:  {}", stats.simulations_passed);
        info!("TXs Submitted:       {}", stats.txs_submitted);
        info!("TXs Confirmed:       {}", stats.txs_confirmed);
        info!("TXs Reverted:        {}", stats.txs_reverted);
        info!("Win Rate:            {:.1}%", win_rate);
        info!("Gross Profit:        {} ETH", ethers::utils::format_ether(stats.total_profit_wei));
        info!("Gas Spent:           {} ETH", ethers::utils::format_ether(stats.total_gas_spent_wei));
        info!("Net Profit:          {} ETH", ethers::utils::format_ether(net_profit));
        info!("═══════════════════════════════════════════");
    }
    
    async fn send_telegram(&self, message: &str) {
        if let (Some(token), Some(chat_id)) = (&self.telegram_bot_token, &self.telegram_chat_id) {
            let url = format!(
                "https://api.telegram.org/bot{}/sendMessage",
                token
            );
            
            let params = serde_json::json!({
                "chat_id": chat_id,
                "text": message,
                "parse_mode": "HTML"
            });
            
            match self.http_client.post(&url).json(&params).send().await {
                Ok(_) => {}
                Err(e) => warn!("Failed to send Telegram alert: {:?}", e),
            }
        }
    }
    
    pub async fn send_alert(&self, message: &str) {
        info!("🚨 ALERT: {}", message);
        self.send_telegram(&format!("🚨 {}", message)).await;
    }
    
    pub async fn send_startup_message(&self) {
        let msg = "🚀 LST Arbitrage Bot Started\n\nMonitoring for opportunities...";
        info!("{}", msg);
        self.send_telegram(msg).await;
    }
}
