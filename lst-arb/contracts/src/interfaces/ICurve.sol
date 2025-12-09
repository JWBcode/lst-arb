// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

interface ICurvePool {
    // Exchange between two coins
    function exchange(
        int128 i,
        int128 j,
        uint256 dx,
        uint256 min_dy
    ) external payable returns (uint256);
    
    // Get expected output amount
    function get_dy(
        int128 i,
        int128 j,
        uint256 dx
    ) external view returns (uint256);
    
    // Get coin addresses
    function coins(uint256 i) external view returns (address);
    
    // Get balances
    function balances(uint256 i) external view returns (uint256);
    
    // Get virtual price (for metapools)
    function get_virtual_price() external view returns (uint256);
}

interface ICurvePoolNG {
    // New generation pools use different signature
    function exchange(
        int128 i,
        int128 j,
        uint256 dx,
        uint256 min_dy,
        address receiver
    ) external returns (uint256);
    
    function get_dy(
        int128 i,
        int128 j,
        uint256 dx
    ) external view returns (uint256);
}

interface ICurveStableSwapNG {
    function exchange(
        int128 i,
        int128 j,
        uint256 _dx,
        uint256 _min_dy
    ) external returns (uint256);
    
    function get_dy(
        int128 i,
        int128 j,
        uint256 _dx
    ) external view returns (uint256);
    
    function coins(uint256 arg0) external view returns (address);
}
