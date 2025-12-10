use ethers::types::{Address, U256};
use std::sync::Arc;
use tracing::{info, debug};

use crate::price::{Quote, Venue, TokenQuotes};
use crate::rpc::WsClient;
use super::solver::{Solver, PoolParams};

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
    solver: Solver,
}

impl OpportunityDetector {
    pub fn new(min_spread_bps: u64, min_profit: U256) -> Self {
        Self {
            min_spread_bps,
            min_profit,
            solver: Solver::new(),
        }
    }

    /// Get reference to the solver for external use
    pub fn solver(&self) -> &Solver {
        &self.solver
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

    /// Detect arbitrage opportunities with optimal trade sizing using convex optimization
    ///
    /// This method uses the solver to calculate mathematically optimal trade sizes
    /// instead of using fixed amounts, and clamps to 90% of Balancer Vault liquidity.
    pub async fn detect_optimal(
        &self,
        client: Arc<WsClient>,
        token_quotes: &[TokenQuotes],
    ) -> Vec<Opportunity> {
        let mut opportunities = Vec::new();

        for tq in token_quotes {
            if let Some(opp) = self.find_optimal_opportunity(client.clone(), tq).await {
                if opp.spread_bps >= self.min_spread_bps && opp.expected_profit >= self.min_profit {
                    opportunities.push(opp);
                }
            }
        }

        // Sort by expected profit (highest first)
        opportunities.sort_by(|a, b| b.expected_profit.cmp(&a.expected_profit));

        opportunities
    }

    /// Find the optimal opportunity for a token using convex optimization
    async fn find_optimal_opportunity(
        &self,
        client: Arc<WsClient>,
        tq: &TokenQuotes,
    ) -> Option<Opportunity> {
        if tq.quotes.len() < 2 {
            return None;
        }

        // Build pool parameters from quotes
        // Use buy/sell amounts as proxy for reserves when liquidity data unavailable
        let pools: Vec<PoolParams> = tq.quotes.iter()
            .filter(|(_, q)| q.buy_amount > U256::zero() || q.sell_amount > U256::zero())
            .map(|(venue, quote)| {
                // Estimate reserve from quote amounts (assuming ~1:1 ratio for LSTs)
                // A quote of X LST for 1 ETH implies reserves of at least X * some_factor
                let estimated_reserve = if quote.buy_amount > U256::zero() {
                    quote.buy_amount * U256::from(100u64) // Conservative estimate
                } else {
                    quote.sell_amount * U256::from(100u64)
                };

                PoolParams {
                    venue: *venue,
                    reserve_x: estimated_reserve,
                    reserve_y: estimated_reserve,
                    fee_bps: venue_fee_bps(*venue),
                    amp: venue_amplification(*venue),
                }
            })
            .collect();

        if pools.len() < 2 {
            return None;
        }

        // Use solver to find optimal trade with liquidity clamping
        let optimal_trade = match self.solver.find_optimal_trade_clamped(client, &pools).await {
            Ok(Some(t)) => t,
            Ok(None) => return None,
            Err(e) => {
                debug!("Solver error for {}: {:?}", tq.token_name, e);
                return None;
            }
        };

        // Convert OptimalTrade to Opportunity
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;

        // Calculate spread in basis points
        let spread_bps = if optimal_trade.optimal_input > U256::zero() {
            optimal_trade.expected_profit
                .checked_mul(U256::from(10000u64))?
                .checked_div(optimal_trade.optimal_input)?
                .as_u64()
        } else {
            0
        };

        // Get buy/sell amounts from quotes for logging (search in Vec)
        let buy_quote = tq.quotes.iter()
            .find(|(v, _)| *v == optimal_trade.buy_venue)
            .map(|(_, q)| q)?;
        let sell_quote = tq.quotes.iter()
            .find(|(v, _)| *v == optimal_trade.sell_venue)
            .map(|(_, q)| q)?;

        Some(Opportunity {
            token: tq.token,
            token_name: tq.token_name.clone(),
            buy_venue: optimal_trade.buy_venue,
            sell_venue: optimal_trade.sell_venue,
            buy_price: buy_quote.buy_amount,
            sell_price: sell_quote.sell_amount,
            spread_bps,
            expected_profit: optimal_trade.expected_profit,
            trade_amount: optimal_trade.optimal_input,
            timestamp_ms,
        })
    }
}

/// Get fee in basis points for each venue
fn venue_fee_bps(venue: Venue) -> u64 {
    match venue {
        Venue::Curve => 4,       // 0.04% for StableSwap
        Venue::Balancer => 10,   // 0.1% typical for Balancer stable pools
        Venue::UniswapV3 => 5,   // 0.05% (lowest tier, LST pairs usually use this)
        Venue::Maverick => 10,   // 0.1% typical
    }
}

/// Get amplification factor for StableSwap venues
fn venue_amplification(venue: Venue) -> Option<u64> {
    match venue {
        Venue::Curve => Some(100),    // Typical A factor for Curve
        Venue::Balancer => Some(200), // Balancer stable pools use higher A
        _ => None,                    // Constant product AMMs don't use A
    }
}

impl Opportunity {
    pub fn log(&self) {
        info!(
            "🎯 OPPORTUNITY: {} | Buy {} @ {:?} | Sell @ {:?} | Spread: {}bps | Profit: {} ETH | Size: {} ETH",
            self.token_name,
            self.token,
            self.buy_venue,
            self.sell_venue,
            self.spread_bps,
            ethers::utils::format_ether(self.expected_profit),
            ethers::utils::format_ether(self.trade_amount)
        );
    }
}
