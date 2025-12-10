#!/bin/bash
set -e

# LST/LRT Arbitrage Contract Deployment Script - Arbitrum One

echo "═══════════════════════════════════════════"
echo "  LST/LRT Arbitrage Contract Deployment"
echo "  Network: Arbitrum One (Chain ID: 42161)"
echo "═══════════════════════════════════════════"

# Check for required environment variables
if [ -z "$PRIVATE_KEY" ]; then
    echo "Error: PRIVATE_KEY not set"
    exit 1
fi

if [ -z "$ARB_RPC_URL" ]; then
    echo "Error: ARB_RPC_URL not set"
    echo "Example: export ARB_RPC_URL=https://arb1.arbitrum.io/rpc"
    exit 1
fi

# Navigate to contracts directory
cd "$(dirname "$0")/../contracts"

echo "Installing Foundry dependencies..."
forge install

echo "Compiling contracts..."
forge build --optimize --optimizer-runs 1000000

echo "Deploying LstArbitrage contract to Arbitrum One..."
DEPLOYED=$(forge create \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY" \
    --verify \
    --verifier-url "https://api.arbiscan.io/api" \
    --etherscan-api-key "${ARBISCAN_API_KEY:-}" \
    src/LstArbitrage.sol:LstArbitrage)

# Extract contract address
CONTRACT_ADDRESS=$(echo "$DEPLOYED" | grep "Deployed to:" | awk '{print $3}')

echo "═══════════════════════════════════════════"
echo "  CONTRACT DEPLOYED!"
echo "  Address: $CONTRACT_ADDRESS"
echo "═══════════════════════════════════════════"

# Configure venues
echo "Configuring venues..."

# Arbitrum token addresses
WSTETH="0x5979D7b546E38E41137eFe97697CBca551Db098E"
RETH="0xEC70Dcb4A1EfA46b8F2D97C310C9c4790bA5ffA8"
CBETH="0x1DEBd73E752bEaF79865Fd6446b0c970EaE7732f"
WEETH="0x35751007a407ca6feffe80b3cb397736d2cf4dbe"
EZETH="0x2416092f143378750bb29b79ed961ab195cceea5"

# Arbitrum venue addresses
CURVE_WSTETH_POOL="0x6eB2dc694eB516B16Dc9d7671f465248B71E9091"
CURVE_RETH_POOL="0x0000000000000000000000000000000000000000"  # Low liquidity - disabled

# Configure Curve wstETH pool (wstETH/ETH NG Pool)
cast send "$CONTRACT_ADDRESS" \
    "configureCurve(address,address)" \
    "$WSTETH" \
    "$CURVE_WSTETH_POOL" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Curve wstETH/ETH pool configured"

# Skip Curve rETH pool configuration if pool address is zero
if [ "$CURVE_RETH_POOL" != "0x0000000000000000000000000000000000000000" ]; then
    cast send "$CONTRACT_ADDRESS" \
        "configureCurve(address,address)" \
        "$RETH" \
        "$CURVE_RETH_POOL" \
        --rpc-url "$ARB_RPC_URL" \
        --private-key "$PRIVATE_KEY"
    echo "  ✓ Curve rETH pool configured"
else
    echo "  ⚠ Curve rETH pool skipped (low liquidity on Arbitrum)"
fi

# Configure Uniswap V3 for wstETH (0.05% fee tier)
# Placeholder pool - update with actual Arbitrum pool address
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "$WSTETH" \
    "0x35218a1cbaC5Bbc3E57fd9Bd38219D37571b3537" \
    "500" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 wstETH pool configured"

# Configure Balancer wstETH pool
cast send "$CONTRACT_ADDRESS" \
    "configureBalancer(address,bytes32)" \
    "$WSTETH" \
    "0x9791d590788598535278552eecd4b211bfc790cb000000000000000000000498" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Balancer wstETH pool configured"

# Configure weETH on Uniswap V3 (500 fee tier - placeholder, update with actual pool)
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "$WEETH" \
    "0x0000000000000000000000000000000000000000" \
    "500" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 weETH pool configured (placeholder - update with actual pool)"

# Configure ezETH on Uniswap V3 (500 fee tier - placeholder, update with actual pool)
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "$EZETH" \
    "0x0000000000000000000000000000000000000000" \
    "500" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 ezETH pool configured (placeholder - update with actual pool)"

echo ""
echo "═══════════════════════════════════════════"
echo "  DEPLOYMENT COMPLETE!"
echo "═══════════════════════════════════════════"
echo ""
echo "Contract Address: $CONTRACT_ADDRESS"
echo "Network: Arbitrum One (42161)"
echo ""
echo "Next steps:"
echo "1. Update ARB_CONTRACT in your .env file"
echo "2. Fund contract with gas (send ETH on Arbitrum)"
echo "3. Update Uniswap pool addresses for weETH/ezETH"
echo "4. Start the bot: cargo run --release"
echo ""
echo "View on Arbiscan: https://arbiscan.io/address/$CONTRACT_ADDRESS"
echo ""
