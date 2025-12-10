use ethers::types::{Address, U256};
use std::sync::Arc;
use crate::rpc::WsClient;

// Note: Direct Curve queries are not used in production.
// The main code path uses multicall.rs for batched queries.
// This module is kept for testing/reference purposes.

pub struct CurveQuoter {
    pool: Address,
}

impl CurveQuoter {
    pub fn new(pool: Address) -> Self {
        Self { pool }
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

    pub async fn get_liquidity(
        &self,
        _client: Arc<WsClient>,
    ) -> eyre::Result<(U256, U256)> {
        // Use multicall.rs for actual queries
        Err(eyre::eyre!("Use MulticallQuoter for batched queries"))
    }
}
