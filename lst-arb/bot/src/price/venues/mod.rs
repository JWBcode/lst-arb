// Individual venue implementations if needed for direct queries
// The main speed path uses multicall, but these are useful for testing

use ethers::prelude::*;
use ethers::types::{Address, U256};
use std::sync::Arc;

use crate::rpc::WsClient;

pub mod curve;
pub mod balancer;
pub mod uniswap;

pub use curve::CurveQuoter;
pub use balancer::BalancerQuoter;
pub use uniswap::UniswapQuoter;
