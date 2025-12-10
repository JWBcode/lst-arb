// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./interfaces/IBalancerVault.sol";

/**
 * @title LstArbitrage (Universal Router)
 * @notice Generic batch executor for zero-capital arbitrage using Balancer flash loans
 * @dev Executes arbitrary call sequences - works with any DEX on Arbitrum
 *      (Uniswap, Curve, Balancer, Camelot, Trader Joe, SushiSwap, etc.)
 */
contract LstArbitrage is IFlashLoanRecipient {
    // ============================================
    // CONSTANTS
    // ============================================

    IBalancerVault public constant BALANCER = IBalancerVault(0xBA12222222228d8Ba445958a75a0704d566BF2C8);
    address public constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1; // Arbitrum WETH

    // ============================================
    // TYPES
    // ============================================

    /**
     * @notice A single execution step in a batch
     * @param target Contract address to call
     * @param callData Encoded function call (selector + params)
     * @param value ETH value to send with call (0 for most DEX calls)
     */
    struct Step {
        address target;
        bytes callData;
        uint256 value;
    }

    /**
     * @notice Flash loan parameters
     * @param tokens Tokens to borrow
     * @param amounts Amounts to borrow
     * @param steps Execution steps to run after receiving loan
     * @param minProfit Minimum profit required (reverts if not met)
     * @param profitToken Token to measure profit in (usually WETH)
     */
    struct FlashLoanParams {
        address[] tokens;
        uint256[] amounts;
        Step[] steps;
        uint256 minProfit;
        address profitToken;
    }

    // ============================================
    // STATE
    // ============================================

    address public immutable owner;

    // Approved targets for security (prevent arbitrary calls)
    mapping(address => bool) public approvedTargets;

    // ============================================
    // EVENTS
    // ============================================

    event BatchExecuted(uint256 stepsExecuted, uint256 gasUsed);
    event FlashLoanExecuted(address[] tokens, uint256[] amounts, uint256 profit);
    event TargetApproved(address indexed target, bool approved);
    event TokenApproved(address indexed token, address indexed spender);

    // ============================================
    // ERRORS
    // ============================================

    error NotOwner();
    error NotBalancer();
    error StepFailed(uint256 index, bytes reason);
    error InsufficientProfit(uint256 actual, uint256 required);
    error TargetNotApproved(address target);
    error InvalidParams();

    // ============================================
    // CONSTRUCTOR
    // ============================================

    constructor() {
        owner = msg.sender;

        // Pre-approve common Arbitrum DEX routers
        _approveTarget(0xBA12222222228d8Ba445958a75a0704d566BF2C8); // Balancer Vault
        _approveTarget(0xE592427A0AEce92De3Edee1F18E0157C05861564); // Uniswap V3 Router
        _approveTarget(0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45); // Uniswap V3 Router 02
        _approveTarget(0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506); // SushiSwap Router
        _approveTarget(0xc873fEcbd354f5A56E00E710B90EF4201db2448d); // Camelot Router
        _approveTarget(0x82aF49447D8a07e3bd95BD0d56f35241523fBab1); // WETH (for wrap/unwrap)

        // Approve Balancer vault for flash loan repayment
        IERC20(WETH).approve(address(BALANCER), type(uint256).max);
    }

    // ============================================
    // MODIFIERS
    // ============================================

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    // ============================================
    // ADMIN FUNCTIONS
    // ============================================

    /**
     * @notice Approve a target contract for batch execution
     * @param target Contract address to approve
     * @param approved Whether to approve or revoke
     */
    function setApprovedTarget(address target, bool approved) external onlyOwner {
        approvedTargets[target] = approved;
        emit TargetApproved(target, approved);
    }

    /**
     * @notice Approve token spending for a DEX router
     * @param token Token to approve
     * @param spender Router/contract to approve
     */
    function approveToken(address token, address spender) external onlyOwner {
        IERC20(token).approve(spender, type(uint256).max);
        emit TokenApproved(token, spender);
    }

    /**
     * @notice Batch approve multiple tokens for a spender
     * @param tokens Array of tokens to approve
     * @param spender Router/contract to approve
     */
    function batchApproveTokens(address[] calldata tokens, address spender) external onlyOwner {
        for (uint256 i = 0; i < tokens.length; i++) {
            IERC20(tokens[i]).approve(spender, type(uint256).max);
            emit TokenApproved(tokens[i], spender);
        }
    }

    /**
     * @notice Withdraw tokens to owner
     * @param token Token address (use address(0) for ETH)
     */
    function withdraw(address token) external onlyOwner {
        if (token == address(0)) {
            uint256 balance = address(this).balance;
            if (balance > 0) {
                payable(owner).transfer(balance);
            }
        } else {
            uint256 balance = IERC20(token).balanceOf(address(this));
            if (balance > 0) {
                IERC20(token).transfer(owner, balance);
            }
        }
    }

    // ============================================
    // BATCH EXECUTION (No Flash Loan)
    // ============================================

    /**
     * @notice Execute a batch of calls without flash loan
     * @dev Use this for simple swaps where you have the capital
     * @param steps Array of execution steps
     */
    function executeBatch(Step[] calldata steps) external onlyOwner {
        uint256 gasStart = gasleft();

        _executeSteps(steps);

        emit BatchExecuted(steps.length, gasStart - gasleft());
    }

    // ============================================
    // FLASH LOAN EXECUTION
    // ============================================

    /**
     * @notice Execute arbitrage with Balancer flash loan (0% fee)
     * @param params Flash loan parameters including steps to execute
     */
    function executeFlashLoan(FlashLoanParams calldata params) external onlyOwner {
        if (params.tokens.length == 0 || params.tokens.length != params.amounts.length) {
            revert InvalidParams();
        }

        // Encode steps and params for callback
        bytes memory userData = abi.encode(params.steps, params.minProfit, params.profitToken);

        // Convert to IERC20 array for Balancer
        IERC20[] memory tokens = new IERC20[](params.tokens.length);
        for (uint256 i = 0; i < params.tokens.length; i++) {
            tokens[i] = IERC20(params.tokens[i]);
        }

        // Request flash loan - callback will execute steps
        BALANCER.flashLoan(this, tokens, params.amounts, userData);
    }

    /**
     * @notice Simplified flash loan for single-token borrows
     * @param token Token to borrow
     * @param amount Amount to borrow
     * @param steps Execution steps
     * @param minProfit Minimum profit required
     */
    function executeFlashLoanSimple(
        address token,
        uint256 amount,
        Step[] calldata steps,
        uint256 minProfit
    ) external onlyOwner {
        bytes memory userData = abi.encode(steps, minProfit, token);

        IERC20[] memory tokens = new IERC20[](1);
        tokens[0] = IERC20(token);

        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount;

        BALANCER.flashLoan(this, tokens, amounts, userData);
    }

    /**
     * @notice Balancer flash loan callback
     * @dev Called by Balancer after sending the loan
     */
    function receiveFlashLoan(
        IERC20[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        if (msg.sender != address(BALANCER)) revert NotBalancer();

        // Decode execution params
        (Step[] memory steps, uint256 minProfit, address profitToken) =
            abi.decode(userData, (Step[], uint256, address));

        // Record balance before execution
        uint256 balanceBefore = IERC20(profitToken).balanceOf(address(this));

        // Execute all steps
        _executeSteps(steps);

        // Repay flash loan(s)
        for (uint256 i = 0; i < tokens.length; i++) {
            uint256 repayAmount = amounts[i] + feeAmounts[i];
            tokens[i].transfer(address(BALANCER), repayAmount);
        }

        // Calculate and verify profit
        uint256 balanceAfter = IERC20(profitToken).balanceOf(address(this));

        // Account for the loan amount that was in our balance during execution
        uint256 profit;
        if (profitToken == address(tokens[0])) {
            // If profit token is the borrowed token, account for loan
            profit = balanceAfter > balanceBefore ? balanceAfter - balanceBefore : 0;
        } else {
            profit = balanceAfter - balanceBefore;
        }

        // ATOMIC REVERT if profit insufficient
        if (profit < minProfit) revert InsufficientProfit(profit, minProfit);

        emit FlashLoanExecuted(_toAddresses(tokens), amounts, profit);
    }

    // ============================================
    // INTERNAL FUNCTIONS
    // ============================================

    /**
     * @notice Execute an array of steps
     * @param steps Array of execution steps
     */
    function _executeSteps(Step[] memory steps) internal {
        for (uint256 i = 0; i < steps.length; i++) {
            Step memory step = steps[i];

            // Security: only allow calls to approved targets
            if (!approvedTargets[step.target]) revert TargetNotApproved(step.target);

            // Execute the call
            (bool success, bytes memory result) = step.target.call{value: step.value}(step.callData);

            if (!success) {
                revert StepFailed(i, result);
            }
        }
    }

    /**
     * @notice Internal function to approve a target
     */
    function _approveTarget(address target) internal {
        approvedTargets[target] = true;
    }

    /**
     * @notice Convert IERC20 array to address array for events
     */
    function _toAddresses(IERC20[] memory tokens) internal pure returns (address[] memory) {
        address[] memory addrs = new address[](tokens.length);
        for (uint256 i = 0; i < tokens.length; i++) {
            addrs[i] = address(tokens[i]);
        }
        return addrs;
    }

    // ============================================
    // VIEW FUNCTIONS
    // ============================================

    /**
     * @notice Check if a target is approved
     */
    function isApprovedTarget(address target) external view returns (bool) {
        return approvedTargets[target];
    }

    /**
     * @notice Get contract's token balance
     */
    function getBalance(address token) external view returns (uint256) {
        return IERC20(token).balanceOf(address(this));
    }

    // ============================================
    // RECEIVE ETH
    // ============================================

    receive() external payable {}
}

// ============================================
// HELPER INTERFACES
// ============================================

interface IWETH {
    function deposit() external payable;
    function withdraw(uint256) external;
}
