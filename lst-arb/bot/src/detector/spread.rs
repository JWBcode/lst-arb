use ethers::types::{Address, U256};
use tracing::{info, debug};

use crate::price::{PriceCache, Quote, Venue, TokenQuotes};

#[derive(Debug, Clone)]
pub struct Opportunity {
    pub token: Address,
    pub token_name: String,
    pub buy_venue: Venue,
    pub sell_venue: Venue,
    pub buy_price: U256,      // LST received per ETH
    pub sell_price: U256,     // ETH received per LST
    pub spread_bps: u64,
    pub expected_profit: U256,
    pub trade_amount: U256,
    pub timestamp_ms: u64,
}

pub struct OpportunityDetector {
    min_spread_bps: u64,
    min_profit: U256,
    max_trade_size: U256,
}

impl OpportunityDetector {
    pub fn new(min_spread_bps: u64, min_profit: U256, max_trade_size: U256) -> Self {
        Self {
            min_spread_bps,
            min_profit,
            max_trade_size,
        }
    }
    
    /// Detect arbitrage opportunities from token quotes
    pub fn detect(&self, token_quotes: &[TokenQuotes], trade_amount: U256) -> Vec<Opportunity> {
        let mut opportunities = Vec::new();
        
        for tq in token_quotes {
            if let Some(opp) = self.find_best_opportunity(tq, trade_amount) {
                if opp.spread_bps >= self.min_spread_bps && opp.expected_profit >= self.min_profit {
                    opportunities.push(opp);
                }
            }
        }
        
        // Sort by expected profit (highest first)
        opportunities.sort_by(|a, b| b.expected_profit.cmp(&a.expected_profit));
        
        opportunities
    }
    
    fn find_best_opportunity(&self, tq: &TokenQuotes, trade_amount: U256) -> Option<Opportunity> {
        if tq.quotes.len() < 2 {
            return None;
        }
        
        // Find best buy venue (highest LST per ETH)
        let best_buy = tq.quotes.iter()
            .filter(|(_, q)| q.buy_amount > U256::zero())
            .max_by_key(|(_, q)| q.buy_amount);
        
        // Find best sell venue (highest ETH per LST)
        let best_sell = tq.quotes.iter()
            .filter(|(_, q)| q.sell_amount > U256::zero())
            .max_by_key(|(_, q)| q.sell_amount);
        
        match (best_buy, best_sell) {
            (Some((buy_venue, buy_quote)), Some((sell_venue, sell_quote))) => {
                // Skip if same venue
                if buy_venue == sell_venue {
                    // Try second best for sell
                    let second_best_sell = tq.quotes.iter()
                        .filter(|(v, q)| v != buy_venue && q.sell_amount > U256::zero())
                        .max_by_key(|(_, q)| q.sell_amount);
                    
                    if let Some((sell_v, sell_q)) = second_best_sell {
                        return self.calculate_opportunity(
                            tq.token,
                            &tq.token_name,
                            *buy_venue,
                            *sell_v,
                            buy_quote,
                            sell_q,
                            trade_amount,
                        );
                    }
                    return None;
                }
                
                self.calculate_opportunity(
                    tq.token,
                    &tq.token_name,
                    *buy_venue,
                    *sell_venue,
                    buy_quote,
                    sell_quote,
                    trade_amount,
                )
            }
            _ => None,
        }
    }
    
    fn calculate_opportunity(
        &self,
        token: Address,
        token_name: &str,
        buy_venue: Venue,
        sell_venue: Venue,
        buy_quote: &Quote,
        sell_quote: &Quote,
        trade_amount: U256,
    ) -> Option<Opportunity> {
        // Calculate spread:
        // Buy: We spend `trade_amount` ETH, get `buy_amount` LST
        // Sell: We sell `buy_amount` LST, get some ETH back
        // Profit = ETH_out - ETH_in
        
        let lst_received = buy_quote.buy_amount;
        if lst_received.is_zero() {
            return None;
        }
        
        // Scale sell_amount proportionally
        // sell_quote.sell_amount is ETH received for `trade_amount` worth of LST
        // We need ETH received for `lst_received` LST
        
        // Simplified calculation assuming linear pricing:
        // sell_amount is already based on trade_amount input
        // For more accuracy, we'd need to re-quote with exact LST amount
        let eth_received = sell_quote.sell_amount;
        
        if eth_received <= trade_amount {
            return None; // No profit
        }
        
        let profit = eth_received - trade_amount;
        
        // Calculate spread in basis points
        // spread = (eth_received - trade_amount) / trade_amount * 10000
        let spread_bps = profit
            .checked_mul(U256::from(10000u64))?
            .checked_div(trade_amount)?
            .as_u64();
        
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;
        
        Some(Opportunity {
            token,
            token_name: token_name.to_string(),
            buy_venue,
            sell_venue,
            buy_price: lst_received,
            sell_price: eth_received,
            spread_bps,
            expected_profit: profit,
            trade_amount,
            timestamp_ms,
        })
    }
}

impl Opportunity {
    pub fn log(&self) {
        info!(
            "🎯 OPPORTUNITY: {} | Buy {} @ {:?} | Sell @ {:?} | Spread: {}bps | Profit: {} ETH",
            self.token_name,
            self.token,
            self.buy_venue,
            self.sell_venue,
            self.spread_bps,
            ethers::utils::format_ether(self.expected_profit)
        );
    }
}
