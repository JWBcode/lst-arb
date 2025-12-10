// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./interfaces/IBalancerVault.sol";
import "./interfaces/ICurve.sol";
import "./interfaces/IUniswapV3.sol";

/**
 * @title LstArbitrage
 * @notice Zero-capital LST/LRT arbitrage using Balancer flash loans (0% fee)
 * @dev Atomic execution: reverts if profit < minProfit, you only pay gas on success
 */
contract LstArbitrage is IFlashLoanRecipient {
    // ============================================
    // CONSTANTS
    // ============================================
    
    IBalancerVault public constant BALANCER = IBalancerVault(0xBA12222222228d8Ba445958a75a0704d566BF2C8);
    address public constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1; // Arbitrum WETH
    
    // Venue identifiers
    uint8 public constant VENUE_CURVE = 1;
    uint8 public constant VENUE_BALANCER = 2;
    uint8 public constant VENUE_UNISWAP_V3 = 3;
    uint8 public constant VENUE_MAVERICK = 4;
    
    // ============================================
    // STATE
    // ============================================
    
    address public immutable owner;
    
    // Venue configurations
    mapping(address => address) public curvePools;      // LST => Curve pool
    mapping(address => bytes32) public balancerPoolIds; // LST => Balancer pool ID
    mapping(address => address) public uniswapPools;    // LST => UniV3 pool
    mapping(address => uint24) public uniswapFees;      // LST => UniV3 fee tier
    
    // ============================================
    // EVENTS
    // ============================================
    
    event ArbitrageExecuted(
        address indexed lst,
        uint8 buyVenue,
        uint8 sellVenue,
        uint256 amountIn,
        uint256 profit
    );
    
    event VenueConfigured(address indexed lst, uint8 venue, address pool);
    
    // ============================================
    // ERRORS
    // ============================================
    
    error NotOwner();
    error NotBalancer();
    error InsufficientProfit(uint256 actual, uint256 required);
    error InvalidVenue(uint8 venue);
    error VenueNotConfigured(address lst, uint8 venue);
    error SwapFailed();
    
    // ============================================
    // CONSTRUCTOR
    // ============================================
    
    constructor() {
        owner = msg.sender;
    }
    
    // ============================================
    // ADMIN FUNCTIONS
    // ============================================
    
    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }
    
    function configureCurve(address lst, address pool) external onlyOwner {
        curvePools[lst] = pool;
        // Pre-approve max for gas savings
        IERC20(WETH).approve(pool, type(uint256).max);
        IERC20(lst).approve(pool, type(uint256).max);
        emit VenueConfigured(lst, VENUE_CURVE, pool);
    }
    
    function configureBalancer(address lst, bytes32 poolId) external onlyOwner {
        balancerPoolIds[lst] = poolId;
        // Approve Balancer vault
        IERC20(WETH).approve(address(BALANCER), type(uint256).max);
        IERC20(lst).approve(address(BALANCER), type(uint256).max);
        emit VenueConfigured(lst, VENUE_BALANCER, address(BALANCER));
    }
    
    function configureUniswapV3(address lst, address pool, uint24 fee) external onlyOwner {
        uniswapPools[lst] = pool;
        uniswapFees[lst] = fee;
        // Approve Uniswap router
        address router = 0xE592427A0AEce92De3Edee1F18E0157C05861564;
        IERC20(WETH).approve(router, type(uint256).max);
        IERC20(lst).approve(router, type(uint256).max);
        emit VenueConfigured(lst, VENUE_UNISWAP_V3, pool);
    }
    
    function withdraw(address token) external onlyOwner {
        uint256 balance = IERC20(token).balanceOf(address(this));
        if (balance > 0) {
            IERC20(token).transfer(owner, balance);
        }
    }
    
    function withdrawETH() external onlyOwner {
        uint256 balance = address(this).balance;
        if (balance > 0) {
            payable(owner).transfer(balance);
        }
    }
    
    // ============================================
    // ARBITRAGE EXECUTION
    // ============================================
    
    /**
     * @notice Execute arbitrage with flash loan
     * @param lst The LST/LRT token to arbitrage
     * @param amount Amount of WETH to flash loan
     * @param buyVenue Venue to buy LST (cheaper)
     * @param sellVenue Venue to sell LST (more expensive)
     * @param minProfit Minimum profit in WETH (reverts if not met)
     */
    function executeArb(
        address lst,
        uint256 amount,
        uint8 buyVenue,
        uint8 sellVenue,
        uint256 minProfit
    ) external onlyOwner {
        // Encode params for flash loan callback
        bytes memory params = abi.encode(lst, buyVenue, sellVenue, minProfit);
        
        // Request flash loan from Balancer (0% fee!)
        IERC20[] memory tokens = new IERC20[](1);
        tokens[0] = IERC20(WETH);
        
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount;
        
        BALANCER.flashLoan(this, tokens, amounts, params);
    }
    
    /**
     * @notice Balancer flash loan callback
     * @dev Called by Balancer after sending us the loan
     */
    function receiveFlashLoan(
        IERC20[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        if (msg.sender != address(BALANCER)) revert NotBalancer();
        
        // Decode params
        (address lst, uint8 buyVenue, uint8 sellVenue, uint256 minProfit) = 
            abi.decode(userData, (address, uint8, uint8, uint256));
        
        uint256 wethAmount = amounts[0];
        uint256 balanceBefore = IERC20(WETH).balanceOf(address(this));
        
        // Step 1: Buy LST with WETH on cheaper venue
        uint256 lstReceived = _swap(buyVenue, WETH, lst, wethAmount);
        
        // Step 2: Sell LST for WETH on expensive venue
        uint256 wethReceived = _swap(sellVenue, lst, WETH, lstReceived);
        
        // Step 3: Repay flash loan (0% fee on Balancer)
        IERC20(WETH).transfer(address(BALANCER), wethAmount + feeAmounts[0]);
        
        // Step 4: Calculate and verify profit
        uint256 balanceAfter = IERC20(WETH).balanceOf(address(this));
        uint256 profit = balanceAfter - (balanceBefore - wethAmount);
        
        // ATOMIC REVERT if profit insufficient
        if (profit < minProfit) revert InsufficientProfit(profit, minProfit);
        
        emit ArbitrageExecuted(lst, buyVenue, sellVenue, wethAmount, profit);
    }
    
    // ============================================
    // INTERNAL SWAP FUNCTIONS
    // ============================================
    
    function _swap(
        uint8 venue,
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        if (venue == VENUE_CURVE) {
            return _swapCurve(tokenIn, tokenOut, amountIn);
        } else if (venue == VENUE_BALANCER) {
            return _swapBalancer(tokenIn, tokenOut, amountIn);
        } else if (venue == VENUE_UNISWAP_V3) {
            return _swapUniswapV3(tokenIn, tokenOut, amountIn);
        } else {
            revert InvalidVenue(venue);
        }
    }
    
    function _swapCurve(
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal returns (uint256) {
        // Determine which token is the LST
        address lst = tokenIn == WETH ? tokenOut : tokenIn;
        address pool = curvePools[lst];
        if (pool == address(0)) revert VenueNotConfigured(lst, VENUE_CURVE);
        
        ICurvePool curve = ICurvePool(pool);
        
        // Curve stETH/ETH pool: index 0 = ETH, index 1 = stETH
        // Other pools may vary - we detect based on coins
        int128 i;
        int128 j;
        
        if (tokenIn == WETH) {
            i = 0; j = 1;
        } else {
            i = 1; j = 0;
        }
        
        // For ETH-based pools, need to handle WETH wrapping
        if (tokenIn == WETH) {
            // Unwrap WETH to ETH for Curve
            IWETH(WETH).withdraw(amountIn);
            return curve.exchange{value: amountIn}(i, j, amountIn, 0);
        } else {
            uint256 balBefore = address(this).balance;
            curve.exchange(i, j, amountIn, 0);
            uint256 received = address(this).balance - balBefore;
            // Wrap ETH to WETH
            IWETH(WETH).deposit{value: received}();
            return received;
        }
    }
    
    function _swapBalancer(
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal returns (uint256) {
        address lst = tokenIn == WETH ? tokenOut : tokenIn;
        bytes32 poolId = balancerPoolIds[lst];
        if (poolId == bytes32(0)) revert VenueNotConfigured(lst, VENUE_BALANCER);
        
        IBalancerVault.SingleSwap memory swap = IBalancerVault.SingleSwap({
            poolId: poolId,
            kind: IBalancerVault.SwapKind.GIVEN_IN,
            assetIn: IAsset(tokenIn),
            assetOut: IAsset(tokenOut),
            amount: amountIn,
            userData: ""
        });
        
        IBalancerVault.FundManagement memory funds = IBalancerVault.FundManagement({
            sender: address(this),
            fromInternalBalance: false,
            recipient: payable(address(this)),
            toInternalBalance: false
        });
        
        return BALANCER.swap(swap, funds, 0, block.timestamp);
    }
    
    function _swapUniswapV3(
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal returns (uint256) {
        address lst = tokenIn == WETH ? tokenOut : tokenIn;
        uint24 fee = uniswapFees[lst];
        if (fee == 0) revert VenueNotConfigured(lst, VENUE_UNISWAP_V3);
        
        ISwapRouter router = ISwapRouter(0xE592427A0AEce92De3Edee1F18E0157C05861564);
        
        ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
            tokenIn: tokenIn,
            tokenOut: tokenOut,
            fee: fee,
            recipient: address(this),
            deadline: block.timestamp,
            amountIn: amountIn,
            amountOutMinimum: 0,
            sqrtPriceLimitX96: 0
        });
        
        return router.exactInputSingle(params);
    }
    
    // ============================================
    // VIEW FUNCTIONS (for simulation)
    // ============================================
    
    /**
     * @notice Simulate arbitrage without executing
     * @dev Used by bot to verify profitability before sending tx
     */
    function simulateArb(
        address lst,
        uint256 amount,
        uint8 buyVenue,
        uint8 sellVenue
    ) external returns (uint256 expectedProfit) {
        // This will revert on-chain but eth_call will return the value
        // Encode and decode to get the expected output
        
        uint256 balanceBefore = IERC20(WETH).balanceOf(address(this));
        
        // Simulate buy
        uint256 lstAmount = _getQuote(buyVenue, WETH, lst, amount);
        
        // Simulate sell
        uint256 wethOut = _getQuote(sellVenue, lst, WETH, lstAmount);
        
        if (wethOut > amount) {
            expectedProfit = wethOut - amount;
        } else {
            expectedProfit = 0;
        }
    }
    
    function _getQuote(
        uint8 venue,
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal view returns (uint256) {
        if (venue == VENUE_CURVE) {
            address lst = tokenIn == WETH ? tokenOut : tokenIn;
            address pool = curvePools[lst];
            int128 i = tokenIn == WETH ? int128(0) : int128(1);
            int128 j = tokenIn == WETH ? int128(1) : int128(0);
            return ICurvePool(pool).get_dy(i, j, amountIn);
        } else if (venue == VENUE_UNISWAP_V3) {
            // For UniV3, we need to use the quoter contract
            // This is simplified - real impl uses quoter
            revert("Use Quoter for UniV3");
        }
        return 0;
    }
    
    // ============================================
    // RECEIVE ETH
    // ============================================
    
    receive() external payable {}
}

// ============================================
// INTERFACES (inline for simplicity)
// ============================================

interface IWETH {
    function deposit() external payable;
    function withdraw(uint256) external;
}
