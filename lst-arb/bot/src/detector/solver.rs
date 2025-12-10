//! Convex Optimization Solver for Arbitrage Trade Sizing
//!
//! Calculates optimal input 'x' where P'(x) = 0 for:
//! - Constant Product AMMs (Uniswap V2/V3)
//! - StableSwap AMMs (Curve)
//!
//! Includes liquidity clamping for Arbitrum Balancer Vault

use ethers::prelude::*;
use ethers::types::{Address, U256};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::rpc::WsClient;
use crate::price::Venue;

// Arbitrum hardcoded addresses
pub const ARBITRUM_BALANCER_VAULT: &str = "0xBA12222222228d8Ba445958a75a0704d566BF2C8";
pub const ARBITRUM_WETH: &str = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1";

// Maximum percentage of vault liquidity to use (90%)
pub const MAX_LIQUIDITY_PERCENT: u64 = 90;

// Minimum trade size (0.01 ETH)
pub const MIN_TRADE_SIZE_WEI: u64 = 10_000_000_000_000_000;

// Maximum iterations for Newton-Raphson
pub const MAX_ITERATIONS: u32 = 50;

// Convergence threshold (0.1% relative change)
pub const CONVERGENCE_THRESHOLD: f64 = 0.001;

abigen!(
    IERC20,
    r#"[
        function balanceOf(address account) external view returns (uint256)
    ]"#
);

/// Pool parameters for optimization
#[derive(Debug, Clone)]
pub struct PoolParams {
    pub venue: Venue,
    pub reserve_x: U256,  // ETH/WETH reserve
    pub reserve_y: U256,  // LST reserve
    pub fee_bps: u64,     // Fee in basis points (e.g., 30 = 0.3%)
    pub amp: Option<u64>, // Amplification factor for StableSwap
}

/// Optimization result
#[derive(Debug, Clone)]
pub struct OptimalTrade {
    pub optimal_input: U256,
    pub expected_profit: U256,
    pub buy_venue: Venue,
    pub sell_venue: Venue,
    pub iterations: u32,
}

/// Convex Optimization Solver
pub struct Solver {
    balancer_vault: Address,
    weth: Address,
}

impl Solver {
    pub fn new() -> Self {
        Self {
            balancer_vault: ARBITRUM_BALANCER_VAULT.parse().unwrap(),
            weth: ARBITRUM_WETH.parse().unwrap(),
        }
    }

    /// Fetch WETH balance from Arbitrum Balancer Vault for liquidity clamping
    pub async fn fetch_vault_weth_balance(&self, client: Arc<WsClient>) -> eyre::Result<U256> {
        let weth_contract = IERC20::new(self.weth, client);
        let balance = weth_contract.balance_of(self.balancer_vault).call().await?;
        debug!(
            "Balancer Vault WETH balance: {} ETH",
            ethers::utils::format_ether(balance)
        );
        Ok(balance)
    }

    /// Clamp trade size to 90% of vault liquidity
    pub fn clamp_to_liquidity(&self, optimal: U256, vault_balance: U256) -> U256 {
        let max_trade = vault_balance * MAX_LIQUIDITY_PERCENT / 100;

        if optimal > max_trade {
            debug!(
                "Clamping trade from {} to {} ETH (90% of vault)",
                ethers::utils::format_ether(optimal),
                ethers::utils::format_ether(max_trade)
            );
            max_trade
        } else {
            optimal
        }
    }

    /// Calculate optimal trade size for Constant Product AMM (Uniswap V2/V3)
    ///
    /// For Constant Product: x * y = k
    /// Output for input dx: dy = y * dx / (x + dx)
    ///
    /// Profit P(dx) = sell_output - dx
    /// where sell_output = sell(buy(dx))
    ///
    /// P'(dx) = 0 gives optimal input
    pub fn optimal_constant_product(
        &self,
        buy_pool: &PoolParams,
        sell_pool: &PoolParams,
    ) -> Option<OptimalTrade> {
        // Convert to f64 for numerical optimization
        let buy_x = u256_to_f64(buy_pool.reserve_x)?;
        let buy_y = u256_to_f64(buy_pool.reserve_y)?;
        let sell_x = u256_to_f64(sell_pool.reserve_y)?; // Note: LST is "x" in sell pool
        let sell_y = u256_to_f64(sell_pool.reserve_x)?; // ETH is "y" in sell pool

        // Fee multipliers (1 - fee)
        let buy_fee = 1.0 - (buy_pool.fee_bps as f64 / 10000.0);
        let sell_fee = 1.0 - (sell_pool.fee_bps as f64 / 10000.0);

        // For two constant product pools:
        // Profit P(x) = sell_fee * sell_y * (buy_fee * buy_y * x / (buy_x + buy_fee * x))
        //               / (sell_x + buy_fee * buy_y * x / (buy_x + buy_fee * x)) - x
        //
        // Optimal x* using closed-form solution:
        // x* = (sqrt(buy_fee * sell_fee * buy_y * sell_y * buy_x * sell_x) - buy_x * sell_x)
        //      / (buy_fee * sell_fee * buy_y + sell_x)

        let sqrt_term = (buy_fee * sell_fee * buy_y * sell_y * buy_x * sell_x).sqrt();
        let numerator = sqrt_term - buy_x * sell_x;
        let denominator = buy_fee * buy_y + sell_x / sell_fee;

        if numerator <= 0.0 || denominator <= 0.0 {
            debug!("No profitable arbitrage opportunity (numerator or denominator <= 0)");
            return None;
        }

        let optimal_x = numerator / denominator;

        if optimal_x < MIN_TRADE_SIZE_WEI as f64 {
            debug!("Optimal trade size below minimum threshold");
            return None;
        }

        // Calculate expected profit
        let lst_bought = buy_fee * buy_y * optimal_x / (buy_x + buy_fee * optimal_x);
        let eth_received = sell_fee * sell_y * lst_bought / (sell_x + lst_bought);
        let profit = eth_received - optimal_x;

        if profit <= 0.0 {
            return None;
        }

        Some(OptimalTrade {
            optimal_input: f64_to_u256(optimal_x)?,
            expected_profit: f64_to_u256(profit)?,
            buy_venue: buy_pool.venue,
            sell_venue: sell_pool.venue,
            iterations: 1, // Closed-form solution
        })
    }

    /// Calculate optimal trade size for StableSwap AMM (Curve)
    ///
    /// StableSwap invariant: A * n^n * sum(x_i) + D = A * D * n^n + D^(n+1) / (n^n * prod(x_i))
    ///
    /// Uses Newton-Raphson iteration to find optimal x where P'(x) = 0
    pub fn optimal_stableswap(
        &self,
        buy_pool: &PoolParams,
        sell_pool: &PoolParams,
    ) -> Option<OptimalTrade> {
        let amp_buy = buy_pool.amp.unwrap_or(100) as f64;
        let amp_sell = sell_pool.amp.unwrap_or(100) as f64;

        let buy_x = u256_to_f64(buy_pool.reserve_x)?;
        let buy_y = u256_to_f64(buy_pool.reserve_y)?;
        let sell_x = u256_to_f64(sell_pool.reserve_y)?;
        let sell_y = u256_to_f64(sell_pool.reserve_x)?;

        let buy_fee = 1.0 - (buy_pool.fee_bps as f64 / 10000.0);
        let sell_fee = 1.0 - (sell_pool.fee_bps as f64 / 10000.0);

        // Use Newton-Raphson to find optimal x
        // Start with geometric mean of reserves as initial guess
        let mut x = ((buy_x * sell_y) / 1000.0).sqrt();
        x = x.max(MIN_TRADE_SIZE_WEI as f64);

        for i in 0..MAX_ITERATIONS {
            // Calculate output from buy pool (ETH -> LST)
            let lst_bought = stableswap_get_dy(buy_x, buy_y, x * buy_fee, amp_buy)?;

            // Calculate output from sell pool (LST -> ETH)
            let eth_received = stableswap_get_dy(sell_x, sell_y, lst_bought * sell_fee, amp_sell)?;

            // Profit P(x) = eth_received - x
            let profit = eth_received - x;

            // Calculate derivative P'(x) using finite differences
            let dx = x * 0.0001; // Small perturbation
            let lst_bought_plus = stableswap_get_dy(buy_x, buy_y, (x + dx) * buy_fee, amp_buy)?;
            let eth_received_plus = stableswap_get_dy(sell_x, sell_y, lst_bought_plus * sell_fee, amp_sell)?;
            let profit_plus = eth_received_plus - (x + dx);

            let derivative = (profit_plus - profit) / dx;

            // Newton-Raphson update: x_new = x - P'(x) / P''(x)
            // We want P'(x) = 0, so we use gradient descent with adaptive step
            if derivative.abs() < 1e-12 {
                break;
            }

            // Second derivative for Newton-Raphson
            let lst_bought_minus = stableswap_get_dy(buy_x, buy_y, (x - dx) * buy_fee, amp_buy)?;
            let eth_received_minus = stableswap_get_dy(sell_x, sell_y, lst_bought_minus * sell_fee, amp_sell)?;
            let profit_minus = eth_received_minus - (x - dx);

            let second_derivative = (profit_plus - 2.0 * profit + profit_minus) / (dx * dx);

            let x_new = if second_derivative.abs() > 1e-12 {
                x - derivative / second_derivative
            } else {
                // Fall back to gradient descent if second derivative is too small
                x + derivative * 0.1 * x
            };

            // Ensure x stays positive
            let x_new = x_new.max(MIN_TRADE_SIZE_WEI as f64);

            // Check for convergence
            if ((x_new - x) / x).abs() < CONVERGENCE_THRESHOLD {
                // Verify this is profitable
                let final_lst = stableswap_get_dy(buy_x, buy_y, x_new * buy_fee, amp_buy)?;
                let final_eth = stableswap_get_dy(sell_x, sell_y, final_lst * sell_fee, amp_sell)?;
                let final_profit = final_eth - x_new;

                if final_profit > 0.0 {
                    return Some(OptimalTrade {
                        optimal_input: f64_to_u256(x_new)?,
                        expected_profit: f64_to_u256(final_profit)?,
                        buy_venue: buy_pool.venue,
                        sell_venue: sell_pool.venue,
                        iterations: i + 1,
                    });
                }
                return None;
            }

            x = x_new;
        }

        // If we didn't converge, check if last x is profitable
        let final_lst = stableswap_get_dy(buy_x, buy_y, x * buy_fee, amp_buy)?;
        let final_eth = stableswap_get_dy(sell_x, sell_y, final_lst * sell_fee, amp_sell)?;
        let final_profit = final_eth - x;

        if final_profit > 0.0 && x >= MIN_TRADE_SIZE_WEI as f64 {
            Some(OptimalTrade {
                optimal_input: f64_to_u256(x)?,
                expected_profit: f64_to_u256(final_profit)?,
                buy_venue: buy_pool.venue,
                sell_venue: sell_pool.venue,
                iterations: MAX_ITERATIONS,
            })
        } else {
            None
        }
    }

    /// Find optimal trade across all venue combinations
    pub fn find_optimal_trade(
        &self,
        pools: &[PoolParams],
    ) -> Option<OptimalTrade> {
        let mut best_trade: Option<OptimalTrade> = None;

        // Try all combinations of buy/sell venues
        for buy_pool in pools {
            for sell_pool in pools {
                if buy_pool.venue == sell_pool.venue {
                    continue;
                }

                let trade = match (buy_pool.venue, sell_pool.venue) {
                    // Both are StableSwap (Curve)
                    (Venue::Curve, Venue::Curve) => {
                        self.optimal_stableswap(buy_pool, sell_pool)
                    }
                    // Both are Constant Product
                    (Venue::UniswapV3 | Venue::Balancer, Venue::UniswapV3 | Venue::Balancer) => {
                        self.optimal_constant_product(buy_pool, sell_pool)
                    }
                    // Mixed: Use numerical optimization
                    _ => {
                        self.optimal_mixed(buy_pool, sell_pool)
                    }
                };

                if let Some(t) = trade {
                    match &best_trade {
                        None => best_trade = Some(t),
                        Some(best) if t.expected_profit > best.expected_profit => {
                            best_trade = Some(t)
                        }
                        _ => {}
                    }
                }
            }
        }

        best_trade
    }

    /// Optimal trade for mixed AMM types using numerical gradient descent
    fn optimal_mixed(
        &self,
        buy_pool: &PoolParams,
        sell_pool: &PoolParams,
    ) -> Option<OptimalTrade> {
        let buy_x = u256_to_f64(buy_pool.reserve_x)?;
        let buy_y = u256_to_f64(buy_pool.reserve_y)?;
        let sell_x = u256_to_f64(sell_pool.reserve_y)?;
        let sell_y = u256_to_f64(sell_pool.reserve_x)?;

        let buy_fee = 1.0 - (buy_pool.fee_bps as f64 / 10000.0);
        let sell_fee = 1.0 - (sell_pool.fee_bps as f64 / 10000.0);
        let amp_buy = buy_pool.amp.unwrap_or(100) as f64;
        let amp_sell = sell_pool.amp.unwrap_or(100) as f64;

        // Calculate output based on pool type
        let calc_output = |input: f64, pool: &PoolParams, is_buy: bool| -> Option<f64> {
            let (x, y, amp) = if is_buy {
                (buy_x, buy_y, amp_buy)
            } else {
                (sell_x, sell_y, amp_sell)
            };
            let fee = if is_buy { buy_fee } else { sell_fee };

            match pool.venue {
                Venue::Curve => stableswap_get_dy(x, y, input * fee, amp),
                _ => Some(fee * y * input / (x + fee * input)), // Constant product
            }
        };

        // Golden section search for optimal x
        let mut a = MIN_TRADE_SIZE_WEI as f64;
        let mut b = buy_x.min(sell_y) * 0.5; // Cap at 50% of smaller reserve
        let phi = (1.0 + 5.0_f64.sqrt()) / 2.0;

        for _ in 0..MAX_ITERATIONS {
            let c = b - (b - a) / phi;
            let d = a + (b - a) / phi;

            let profit_c = {
                let lst = calc_output(c, buy_pool, true)?;
                let eth = calc_output(lst, sell_pool, false)?;
                eth - c
            };

            let profit_d = {
                let lst = calc_output(d, buy_pool, true)?;
                let eth = calc_output(lst, sell_pool, false)?;
                eth - d
            };

            if profit_c > profit_d {
                b = d;
            } else {
                a = c;
            }

            if (b - a).abs() < MIN_TRADE_SIZE_WEI as f64 {
                break;
            }
        }

        let optimal_x = (a + b) / 2.0;
        let lst_bought = calc_output(optimal_x, buy_pool, true)?;
        let eth_received = calc_output(lst_bought, sell_pool, false)?;
        let profit = eth_received - optimal_x;

        if profit > 0.0 && optimal_x >= MIN_TRADE_SIZE_WEI as f64 {
            Some(OptimalTrade {
                optimal_input: f64_to_u256(optimal_x)?,
                expected_profit: f64_to_u256(profit)?,
                buy_venue: buy_pool.venue,
                sell_venue: sell_pool.venue,
                iterations: MAX_ITERATIONS,
            })
        } else {
            None
        }
    }

    /// Find optimal trade with liquidity clamping
    pub async fn find_optimal_trade_clamped(
        &self,
        client: Arc<WsClient>,
        pools: &[PoolParams],
    ) -> eyre::Result<Option<OptimalTrade>> {
        // Find mathematically optimal trade
        let optimal = match self.find_optimal_trade(pools) {
            Some(t) => t,
            None => return Ok(None),
        };

        // Fetch vault balance for clamping
        let vault_balance = self.fetch_vault_weth_balance(client).await?;

        // Clamp to 90% of vault liquidity
        let clamped_input = self.clamp_to_liquidity(optimal.optimal_input, vault_balance);

        // If significantly clamped, recalculate expected profit
        if clamped_input < optimal.optimal_input {
            debug!(
                "Trade clamped: {} -> {} ETH",
                ethers::utils::format_ether(optimal.optimal_input),
                ethers::utils::format_ether(clamped_input)
            );

            // Return clamped trade (profit will be lower but trade won't revert)
            Ok(Some(OptimalTrade {
                optimal_input: clamped_input,
                expected_profit: optimal.expected_profit * clamped_input / optimal.optimal_input,
                ..optimal
            }))
        } else {
            Ok(Some(optimal))
        }
    }
}

impl Default for Solver {
    fn default() -> Self {
        Self::new()
    }
}

/// StableSwap output calculation
/// D = A * n^n * sum(x_i) + D / (n^n * prod(x_i) / D^n)
fn stableswap_get_dy(x: f64, y: f64, dx: f64, amp: f64) -> Option<f64> {
    // Simplified 2-coin StableSwap
    // D = 2 * A * (x + y) + D - A * D + D^3 / (4 * x * y * D)
    let n = 2.0;
    let ann = amp * n * n;

    // Calculate D using Newton-Raphson
    let s = x + y;
    if s == 0.0 {
        return Some(0.0);
    }

    let mut d = s;
    for _ in 0..256 {
        let d_p = d * d * d / (4.0 * x * y);
        let d_new = (ann * s + d_p * n) * d / ((ann - 1.0) * d + (n + 1.0) * d_p);

        if (d_new - d).abs() < 1.0 {
            d = d_new;
            break;
        }
        d = d_new;
    }

    // Calculate y after swap
    let x_new = x + dx;
    let mut y_new = d;
    let c = d * d * d / (4.0 * ann * x_new);
    let b = x_new + d / ann;

    for _ in 0..256 {
        let y_prev = y_new;
        y_new = (y_new * y_new + c) / (2.0 * y_new + b - d);

        if (y_new - y_prev).abs() < 1.0 {
            break;
        }
    }

    if y > y_new {
        Some(y - y_new)
    } else {
        Some(0.0)
    }
}

/// Convert U256 to f64 (with precision loss for large numbers)
fn u256_to_f64(val: U256) -> Option<f64> {
    // Handle the conversion carefully to avoid overflow
    let mut result = 0.0f64;
    let mut val = val;
    let base: f64 = 2.0_f64.powi(64);

    for i in 0..4 {
        let limb = val.low_u64();
        result += (limb as f64) * base.powi(i);
        val = val >> 64;
    }

    if result.is_finite() {
        Some(result)
    } else {
        None
    }
}

/// Convert f64 to U256
fn f64_to_u256(val: f64) -> Option<U256> {
    if val < 0.0 || !val.is_finite() {
        return None;
    }

    if val > u128::MAX as f64 {
        // Handle very large numbers
        let high = (val / (2.0_f64.powi(128))) as u128;
        let low = (val % (2.0_f64.powi(128))) as u128;
        Some(U256::from(high) << 128 | U256::from(low))
    } else {
        Some(U256::from(val as u128))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_product_optimization() {
        let solver = Solver::new();

        let buy_pool = PoolParams {
            venue: Venue::UniswapV3,
            reserve_x: ethers::utils::parse_ether("1000.0").unwrap(), // 1000 ETH
            reserve_y: ethers::utils::parse_ether("950.0").unwrap(),  // 950 LST (cheaper to buy)
            fee_bps: 30, // 0.3%
            amp: None,
        };

        let sell_pool = PoolParams {
            venue: Venue::Balancer,
            reserve_x: ethers::utils::parse_ether("500.0").unwrap(), // 500 ETH
            reserve_y: ethers::utils::parse_ether("480.0").unwrap(), // 480 LST (more expensive)
            fee_bps: 30,
            amp: None,
        };

        let result = solver.optimal_constant_product(&buy_pool, &sell_pool);

        if let Some(trade) = result {
            println!("Optimal input: {} ETH", ethers::utils::format_ether(trade.optimal_input));
            println!("Expected profit: {} ETH", ethers::utils::format_ether(trade.expected_profit));
            assert!(trade.expected_profit > U256::zero());
        }
    }

    #[test]
    fn test_liquidity_clamping() {
        let solver = Solver::new();

        let optimal = ethers::utils::parse_ether("100.0").unwrap();
        let vault_balance = ethers::utils::parse_ether("50.0").unwrap();

        let clamped = solver.clamp_to_liquidity(optimal, vault_balance);

        // Should be 90% of 50 = 45 ETH
        let expected = ethers::utils::parse_ether("45.0").unwrap();
        assert_eq!(clamped, expected);
    }
}
