// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./interfaces/IBalancerVault.sol";
import "./interfaces/ICurve.sol";
import "./interfaces/IUniswapV3.sol";

contract LstArbitrage is IFlashLoanRecipient {
    IBalancerVault public constant BALANCER = IBalancerVault(0xBA12222222228d8Ba445958a75a0704d566BF2C8);
    address public constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;

    // Venue IDs matched to Rust bot
    uint8 public constant VENUE_CURVE = 1;
    uint8 public constant VENUE_BALANCER = 2;
    uint8 public constant VENUE_UNISWAP_V3 = 3;

    address public immutable owner;

    // Storage for pool configurations
    mapping(address => address) public curvePools;
    mapping(address => bytes32) public balancerPoolIds;
    mapping(address => address) public uniswapPools;
    mapping(address => uint24) public uniswapFees;

    error NotOwner();
    error NotBalancer();
    error InsufficientProfit(uint256 actual, uint256 required);
    error VenueNotConfigured(address lst, uint8 venue);
    error SwapFailed();

    constructor() {
        owner = msg.sender;
        // Approve WETH to Balancer for flash loan repayment
        IERC20(WETH).approve(address(BALANCER), type(uint256).max);
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    // --- Configuration Functions (Called by deploy.sh) ---

    function configureCurve(address lst, address pool) external onlyOwner {
        curvePools[lst] = pool;
        // Approve Curve to spend LST and WETH
        IERC20(lst).approve(pool, type(uint256).max);
        IERC20(WETH).approve(pool, type(uint256).max);
    }

    function configureBalancer(address lst, bytes32 poolId) external onlyOwner {
        balancerPoolIds[lst] = poolId;
        // Approve Balancer Vault
        IERC20(lst).approve(address(BALANCER), type(uint256).max);
    }

    function configureUniswapV3(address lst, address pool, uint24 fee) external onlyOwner {
        uniswapPools[lst] = pool;
        uniswapFees[lst] = fee;
        // Approve Uniswap Router (0xE592...)
        address router = 0xE592427A0AEce92De3Edee1F18E0157C05861564;
        IERC20(lst).approve(router, type(uint256).max);
        IERC20(WETH).approve(router, type(uint256).max);
    }

    function withdraw(address token) external onlyOwner {
        IERC20(token).transfer(owner, IERC20(token).balanceOf(address(this)));
    }

    // --- Arbitrage Execution (Called by Bot) ---

    struct ArbParams {
        address lst;
        uint256 amount; // Amount of WETH to borrow
        uint8 buyVenue;
        uint8 sellVenue;
        uint256 minProfit;
    }

    function executeArb(
        address lst,
        uint256 amount,
        uint8 buyVenue,
        uint8 sellVenue,
        uint256 minProfit
    ) external onlyOwner {
        // Encode params for flash loan callback
        bytes memory userData = abi.encode(ArbParams({
            lst: lst,
            amount: amount,
            buyVenue: buyVenue,
            sellVenue: sellVenue,
            minProfit: minProfit
        }));

        IERC20[] memory tokens = new IERC20[](1);
        tokens[0] = IERC20(WETH);
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount;

        BALANCER.flashLoan(this, tokens, amounts, userData);
    }

    function receiveFlashLoan(
        IERC20[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        if (msg.sender != address(BALANCER)) revert NotBalancer();

        ArbParams memory params = abi.decode(userData, (ArbParams));
        uint256 balanceBefore = IERC20(WETH).balanceOf(address(this));

        // 1. Buy LST with WETH (WETH -> LST)
        uint256 lstReceived = _swap(params.buyVenue, WETH, params.lst, params.amount);

        // 2. Sell LST for WETH (LST -> WETH)
        _swap(params.sellVenue, params.lst, WETH, lstReceived);

        // 3. Repay Flash Loan
        uint256 repayAmount = amounts[0] + feeAmounts[0];
        IERC20(WETH).transfer(address(BALANCER), repayAmount);

        // 4. Check Profit
        uint256 balanceAfter = IERC20(WETH).balanceOf(address(this));

        uint256 profit = balanceAfter > (balanceBefore - params.amount) ?
            balanceAfter - (balanceBefore - params.amount) : 0;

        if (profit < params.minProfit) revert InsufficientProfit(profit, params.minProfit);
    }

    function _swap(uint8 venue, address tokenIn, address tokenOut, uint256 amount) internal returns (uint256) {
        if (venue == VENUE_CURVE) {
            address pool = curvePools[tokenIn == WETH ? tokenOut : tokenIn];
            if (pool == address(0)) revert VenueNotConfigured(tokenIn, venue);

            int128 i = tokenIn == WETH ? int128(0) : int128(1);
            int128 j = tokenIn == WETH ? int128(1) : int128(0);

            return ICurvePoolNG(pool).exchange(i, j, amount, 0, address(this));
        }
        else if (venue == VENUE_UNISWAP_V3) {
            address tokenForConfig = tokenIn == WETH ? tokenOut : tokenIn;
            uint24 fee = uniswapFees[tokenForConfig];
            if (fee == 0) revert VenueNotConfigured(tokenForConfig, venue);

            ISwapRouter router = ISwapRouter(0xE592427A0AEce92De3Edee1F18E0157C05861564);
            ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
                tokenIn: tokenIn,
                tokenOut: tokenOut,
                fee: fee,
                recipient: address(this),
                deadline: block.timestamp,
                amountIn: amount,
                amountOutMinimum: 0,
                sqrtPriceLimitX96: 0
            });
            return router.exactInputSingle(params);
        }
        else if (venue == VENUE_BALANCER) {
            address lst = tokenIn == WETH ? tokenOut : tokenIn;
            bytes32 poolId = balancerPoolIds[lst];
            if (poolId == bytes32(0)) revert VenueNotConfigured(lst, venue);

            IBalancerVault.SingleSwap memory swapParams = IBalancerVault.SingleSwap({
                poolId: poolId,
                kind: IBalancerVault.SwapKind.GIVEN_IN,
                assetIn: IAsset(tokenIn),
                assetOut: IAsset(tokenOut),
                amount: amount,
                userData: ""
            });

            IBalancerVault.FundManagement memory funds = IBalancerVault.FundManagement({
                sender: address(this),
                fromInternalBalance: false,
                recipient: payable(address(this)),
                toInternalBalance: false
            });

            return BALANCER.swap(swapParams, funds, 0, block.timestamp);
        }
        revert("Invalid Venue");
    }
}
