#!/bin/bash
set -e

# LST/LRT Arbitrage Contract Deployment Script

echo "═══════════════════════════════════════════"
echo "  LST/LRT Arbitrage Contract Deployment"
echo "═══════════════════════════════════════════"

# Check for required environment variables
if [ -z "$PRIVATE_KEY" ]; then
    echo "Error: PRIVATE_KEY not set"
    exit 1
fi

if [ -z "$ETH_RPC_URL" ]; then
    echo "Error: ETH_RPC_URL not set"
    exit 1
fi

# Navigate to contracts directory
cd "$(dirname "$0")/../contracts"

echo "Installing Foundry dependencies..."
forge install

echo "Compiling contracts..."
forge build --optimize --optimizer-runs 1000000

echo "Deploying LstArbitrage contract..."
DEPLOYED=$(forge create \
    --rpc-url "$ETH_RPC_URL" \
    --private-key "$PRIVATE_KEY" \
    --verify \
    --etherscan-api-key "${ETHERSCAN_API_KEY:-}" \
    src/LstArbitrage.sol:LstArbitrage)

# Extract contract address
CONTRACT_ADDRESS=$(echo "$DEPLOYED" | grep "Deployed to:" | awk '{print $3}')

echo "═══════════════════════════════════════════"
echo "  CONTRACT DEPLOYED!"
echo "  Address: $CONTRACT_ADDRESS"
echo "═══════════════════════════════════════════"

# Configure venues
echo "Configuring venues..."

# Configure Curve stETH pool
cast send "$CONTRACT_ADDRESS" \
    "configureCurve(address,address)" \
    "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84" \
    "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022" \
    --rpc-url "$ETH_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Curve stETH pool configured"

# Configure Uniswap V3 for stETH (0.05% fee tier)
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84" \
    "0x4028DAAC072e492d34a3Afdbef0ba7e35D8b55C4" \
    "500" \
    --rpc-url "$ETH_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 stETH pool configured"

# Configure Balancer wstETH pool
cast send "$CONTRACT_ADDRESS" \
    "configureBalancer(address,bytes32)" \
    "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0" \
    "0x32296969ef14eb0c6d29669c550d4a0449130230000200000000000000000080" \
    --rpc-url "$ETH_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Balancer wstETH pool configured"

# Configure weETH (EtherFi LRT)
cast send "$CONTRACT_ADDRESS" \
    "configureUniswapV3(address,address,uint24)" \
    "0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee" \
    "0x7A415B19932c0105c82FDB6b720bb01B0CC2CAe3" \
    "500" \
    --rpc-url "$ETH_RPC_URL" \
    --private-key "$PRIVATE_KEY"

echo "  ✓ Uniswap V3 weETH pool configured"

echo ""
echo "═══════════════════════════════════════════"
echo "  DEPLOYMENT COMPLETE!"
echo "═══════════════════════════════════════════"
echo ""
echo "Contract Address: $CONTRACT_ADDRESS"
echo ""
echo "Next steps:"
echo "1. Update ARB_CONTRACT in your .env file"
echo "2. Fund contract with gas (send ETH)"
echo "3. Start the bot: cargo run --release"
echo ""
