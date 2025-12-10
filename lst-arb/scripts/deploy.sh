#!/bin/bash
set -e

# LST/LRT Arbitrage Contract Deployment Script - Arbitrum One

echo "═══════════════════════════════════════════"
echo "  LST/LRT Arbitrage Contract Deployment"
echo "  Network: Arbitrum One (Chain ID: 42161)"
echo "═══════════════════════════════════════════"

# Expected chain ID for Arbitrum One
EXPECTED_CHAIN_ID=42161

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

# Validate Chain ID before deployment
echo "Validating network connection..."
CHAIN_ID=$(cast chain-id --rpc-url "$ARB_RPC_URL" 2>/dev/null || echo "0")

if [ "$CHAIN_ID" != "$EXPECTED_CHAIN_ID" ]; then
    echo "Error: Chain ID mismatch!"
    echo "  Expected: $EXPECTED_CHAIN_ID (Arbitrum One)"
    echo "  Got: $CHAIN_ID"
    echo ""
    echo "Please ensure ARB_RPC_URL points to an Arbitrum One RPC endpoint."
    exit 1
fi

echo "  ✓ Connected to Arbitrum One (Chain ID: $CHAIN_ID)"

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

# Arbitrum WETH address
WETH="0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"

# Arbitrum token addresses
WSTETH="0x5979D7b546E38E41137eFe97697CBca551Db098E"
RETH="0xEC70Dcb4A1EfA46b8F2D97C310C9c4790bA5ffA8"
CBETH="0x1DEBd73E752bEaF79865Fd6446b0c970EaE7732f"
WEETH="0x35751007a407ca6feffe80b3cb397736d2cf4dbe"
EZETH="0x2416092f143378750bb29b79ed961ab195cceea5"

# Arbitrum Curve pool addresses
CURVE_WSTETH_POOL="0x6eB2dc694eB516B16Dc9d7671f465248B71E9091"  # wstETH/ETH NG Pool
CURVE_RETH_POOL="0x30DF229cefa463e991e29D42DB0bae4e126f2aa9"    # rETH/ETH Pool

# Arbitrum Uniswap V3 pool addresses (0.05% fee tier = 500)
UNISWAP_WSTETH_WETH_POOL="0x35218a1cbaC5Bbc3E57fd9Bd38219D37571b3537"  # wstETH/WETH 0.05%
UNISWAP_WEETH_WETH_POOL="0xd4F4D0a10bCae078FBF7aaAc1270de0696E69fcC"   # weETH/WETH 0.05%
UNISWAP_EZETH_WETH_POOL="0x2905b5e0d6E1F5B234c0c4Bb6a667B5e71c44b22"   # ezETH/WETH 0.05%

# Configure Curve wstETH pool (wstETH/ETH NG Pool)
echo "Configuring Curve wstETH/ETH pool..."
cast send "$CONTRACT_ADDRESS" \
    "configureCurve(address,address)" \
    "$WSTETH" \
    "$CURVE_WSTETH_POOL" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Curve wstETH/ETH pool configured"

# Configure Curve rETH pool
echo "Configuring Curve rETH/ETH pool..."
cast send "$CONTRACT_ADDRESS" \
    "configureCurve(address,address)" \
    "$RETH" \
    "$CURVE_RETH_POOL" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Curve rETH/ETH pool configured"

# Configure Uniswap V3 for wstETH (0.05% fee tier)
echo "Configuring Uniswap V3 wstETH/WETH pool..."
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "$WSTETH" \
    "$UNISWAP_WSTETH_WETH_POOL" \
    "500" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 wstETH/WETH pool configured"

# Configure Balancer wstETH pool
echo "Configuring Balancer wstETH pool..."
cast send "$CONTRACT_ADDRESS" \
    "configureBalancer(address,bytes32)" \
    "$WSTETH" \
    "0x9791d590788598535278552eecd4b211bfc790cb000000000000000000000498" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Balancer wstETH pool configured"

# Configure weETH on Uniswap V3 (0.05% fee tier)
echo "Configuring Uniswap V3 weETH/WETH pool..."
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "$WEETH" \
    "$UNISWAP_WEETH_WETH_POOL" \
    "500" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 weETH/WETH pool configured"

# Configure ezETH on Uniswap V3 (0.05% fee tier)
echo "Configuring Uniswap V3 ezETH/WETH pool..."
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "$EZETH" \
    "$UNISWAP_EZETH_WETH_POOL" \
    "500" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 ezETH/WETH pool configured"

echo ""
echo "═══════════════════════════════════════════"
echo "  DEPLOYMENT COMPLETE!"
echo "═══════════════════════════════════════════"
echo ""
echo "Contract Address: $CONTRACT_ADDRESS"
echo "Network: Arbitrum One (Chain ID: $EXPECTED_CHAIN_ID)"
echo "WETH: $WETH"
echo ""
echo "Configured Venues:"
echo "  - Curve wstETH/ETH: $CURVE_WSTETH_POOL"
echo "  - Curve rETH/ETH: $CURVE_RETH_POOL"
echo "  - Uniswap V3 wstETH/WETH: $UNISWAP_WSTETH_WETH_POOL"
echo "  - Uniswap V3 weETH/WETH: $UNISWAP_WEETH_WETH_POOL"
echo "  - Uniswap V3 ezETH/WETH: $UNISWAP_EZETH_WETH_POOL"
echo "  - Balancer wstETH: Configured"
echo ""
echo "Next steps:"
echo "1. Update ARB_CONTRACT=$CONTRACT_ADDRESS in your .env file"
echo "2. Fund contract with gas (send ETH on Arbitrum)"
echo "3. Start the bot: cargo run --release"
echo ""
echo "View on Arbiscan: https://arbiscan.io/address/$CONTRACT_ADDRESS"
echo ""
