use ethers::types::{Address, U256};
use std::sync::Arc;
use crate::rpc::WsClient;

// Note: Direct Uniswap queries are not used in production.
// The main code path uses multicall.rs for batched queries.
// This module is kept for testing/reference purposes.

pub struct UniswapQuoter {
    quoter: Address,
    weth: Address,
    lst: Address,
    fee: u32,
}

impl UniswapQuoter {
    pub fn new(quoter: Address, weth: Address, lst: Address, fee: u32) -> Self {
        Self { quoter, weth, lst, fee }
    }

    pub async fn get_buy_quote(
        &self,
        _client: Arc<WsClient>,
        _amount_in: U256,
    ) -> eyre::Result<U256> {
        // Use multicall.rs for actual queries
        Err(eyre::eyre!("Use MulticallQuoter for batched queries"))
    }

    pub async fn get_sell_quote(
        &self,
        _client: Arc<WsClient>,
        _amount_in: U256,
    ) -> eyre::Result<U256> {
        // Use multicall.rs for actual queries
        Err(eyre::eyre!("Use MulticallQuoter for batched queries"))
    }

    /// Try multiple fee tiers and return best quote
    pub async fn get_best_buy_quote(
        &self,
        _client: Arc<WsClient>,
        _amount_in: U256,
    ) -> eyre::Result<(U256, u32)> {
        // Use multicall.rs for actual queries
        Err(eyre::eyre!("Use MulticallQuoter for batched queries"))
    }
}
