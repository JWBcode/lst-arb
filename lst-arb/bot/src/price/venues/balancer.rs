use ethers::prelude::*;
use ethers::types::{Address, U256, I256};
use std::sync::Arc;
use crate::rpc::WsClient;

abigen!(
    BalancerVault,
    r#"[
        function queryBatchSwap(uint8 kind, tuple(bytes32 poolId, uint256 assetInIndex, uint256 assetOutIndex, uint256 amount, bytes userData)[] swaps, address[] assets, tuple(address sender, bool fromInternalBalance, address recipient, bool toInternalBalance) funds) external returns (int256[] assetDeltas)
        function getPoolTokens(bytes32 poolId) external view returns (address[] tokens, uint256[] balances, uint256 lastChangeBlock)
    ]"#
);

#[derive(Debug, Clone)]
pub struct BatchSwapStep {
    pub pool_id: [u8; 32],
    pub asset_in_index: U256,
    pub asset_out_index: U256,
    pub amount: U256,
    pub user_data: Bytes,
}

#[derive(Debug, Clone)]
pub struct FundManagement {
    pub sender: Address,
    pub from_internal_balance: bool,
    pub recipient: Address,
    pub to_internal_balance: bool,
}

pub struct BalancerQuoter {
    vault: Address,
    pool_id: [u8; 32],
    weth: Address,
    lst: Address,
}

impl BalancerQuoter {
    pub fn new(vault: Address, pool_id: [u8; 32], weth: Address, lst: Address) -> Self {
        Self { vault, pool_id, weth, lst }
    }
    
    pub async fn get_buy_quote(
        &self,
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        // WETH -> LST
        self.query_swap(client, self.weth, self.lst, amount_in).await
    }
    
    pub async fn get_sell_quote(
        &self,
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        // LST -> WETH
        self.query_swap(client, self.lst, self.weth, amount_in).await
    }
    
    async fn query_swap(
        &self,
        client: Arc<WsClient>,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        let vault = BalancerVault::new(self.vault, client);
        
        // Determine asset indices
        let (asset_in_idx, asset_out_idx) = if token_in < token_out {
            (U256::zero(), U256::one())
        } else {
            (U256::one(), U256::zero())
        };
        
        let assets = if token_in < token_out {
            vec![token_in, token_out]
        } else {
            vec![token_out, token_in]
        };
        
        let swaps = vec![(
            self.pool_id,
            asset_in_idx,
            asset_out_idx,
            amount_in,
            Bytes::default(),
        )];
        
        let funds = (
            Address::zero(),
            false,
            Address::zero(),
            false,
        );
        
        // kind = 0 for GIVEN_IN
        let deltas = vault.query_batch_swap(0u8, swaps, assets, funds).call().await?;
        
        // The output delta is negative (we receive tokens)
        // Return absolute value
        if deltas.len() > 1 {
            let out_delta = deltas[asset_out_idx.as_usize()];
            // Convert negative I256 to positive U256
            if out_delta < I256::zero() {
                let abs_value = out_delta.abs();
                Ok(U256::from_big_endian(&abs_value.into_raw().to_be_bytes::<32>()))
            } else {
                Ok(U256::zero())
            }
        } else {
            Ok(U256::zero())
        }
    }
}
