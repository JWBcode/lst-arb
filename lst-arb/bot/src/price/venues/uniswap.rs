use ethers::prelude::*;
use ethers::types::{Address, U256};
use std::sync::Arc;
use crate::rpc::WsClient;

abigen!(
    QuoterV2,
    r#"[
        function quoteExactInputSingle(tuple(address tokenIn, address tokenOut, uint256 amountIn, uint24 fee, uint160 sqrtPriceLimitX96) params) external returns (uint256 amountOut, uint160 sqrtPriceX96After, uint32 initializedTicksCrossed, uint256 gasEstimate)
    ]"#
);

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
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        // WETH -> LST
        self.quote_exact_input(client, self.weth, self.lst, amount_in).await
    }
    
    pub async fn get_sell_quote(
        &self,
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        // LST -> WETH
        self.quote_exact_input(client, self.lst, self.weth, amount_in).await
    }
    
    async fn quote_exact_input(
        &self,
        client: Arc<WsClient>,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> eyre::Result<U256> {
        let quoter = QuoterV2::new(self.quoter, client);
        
        let params = (
            token_in,
            token_out,
            amount_in,
            self.fee,
            U256::zero(), // sqrtPriceLimitX96
        );
        
        let (amount_out, _, _, _) = quoter.quote_exact_input_single(params).call().await?;
        
        Ok(amount_out)
    }
    
    /// Try multiple fee tiers and return best quote
    pub async fn get_best_buy_quote(
        &self,
        client: Arc<WsClient>,
        amount_in: U256,
    ) -> eyre::Result<(U256, u32)> {
        let fee_tiers = [100, 500, 3000, 10000]; // 0.01%, 0.05%, 0.3%, 1%
        let mut best_amount = U256::zero();
        let mut best_fee = 0u32;
        
        for fee in fee_tiers {
            let quoter = QuoterV2::new(self.quoter, client.clone());
            let params = (
                self.weth,
                self.lst,
                amount_in,
                fee,
                U256::zero(),
            );
            
            if let Ok((amount_out, _, _, _)) = quoter.quote_exact_input_single(params).call().await {
                if amount_out > best_amount {
                    best_amount = amount_out;
                    best_fee = fee;
                }
            }
        }
        
        Ok((best_amount, best_fee))
    }
}
