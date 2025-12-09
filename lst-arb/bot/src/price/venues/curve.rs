use ethers::prelude::*;
use ethers::types::{Address, U256};
use std::sync::Arc;
use crate::rpc::WsClient;

abigen!(
    CurvePool,
    r#"[
        function get_dy(int128 i, int128 j, uint256 dx) external view returns (uint256)
        function balances(uint256 i) external view returns (uint256)
        function coins(uint256 i) external view returns (address)
    ]"#
);

pub struct CurveQuoter {
    pool: Address,
}

impl CurveQuoter {
    pub fn new(pool: Address) -> Self {
        Self { pool }
    }
    
    pub async fn get_buy_quote(
        &self,
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        let pool = CurvePool::new(self.pool, client);
        // ETH (0) -> LST (1)
        let amount_out = pool.get_dy(0, 1, amount_in).call().await?;
        Ok(amount_out)
    }
    
    pub async fn get_sell_quote(
        &self,
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        let pool = CurvePool::new(self.pool, client);
        // LST (1) -> ETH (0)
        let amount_out = pool.get_dy(1, 0, amount_in).call().await?;
        Ok(amount_out)
    }
    
    pub async fn get_liquidity(
        &self,
        client: Arc<WsClient>,
    ) -> eyre::Result<(U256, U256)> {
        let pool = CurvePool::new(self.pool, client);
        let eth_balance = pool.balances(U256::zero()).call().await?;
        let lst_balance = pool.balances(U256::one()).call().await?;
        Ok((eth_balance, lst_balance))
    }
}
