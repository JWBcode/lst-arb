# LST/LRT Arbitrage Bot

Zero-capital arbitrage bot for Liquid Staking Tokens (LSTs) and Liquid Restaking Tokens (LRTs) using Balancer flash loans (0% fee).

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     COMPLETE SYSTEM                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   RPC Layer ──▶ Price Engine ──▶ Detector ──▶ Simulator ──▶ Executor
│       │             │               │              │            │
│   Load Balance   Multicall      Spread Calc    eth_call     Flashbots
│   Health Check   All Venues     Profitability  Gas Est      Direct TX
│   Auto Failover  Single RPC     Filtering      Revert Check  Nonce Mgmt
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Performance Targets

| Component | Target Latency |
|-----------|----------------|
| RPC Call (multicall) | <20ms |
| Spread Detection | <5ms |
| Simulation | <30ms |
| TX Submission | <50ms |
| **Total End-to-End** | **<100ms** |

## Supported Tokens

### LSTs (Liquid Staking)
- stETH (Lido)
- rETH (Rocket Pool)
- cbETH (Coinbase)
- wstETH (Wrapped stETH)

### LRTs (Liquid Restaking) - Higher Spreads!
- weETH (EtherFi)
- ezETH (Renzo)
- rsETH (Kelp)

## Supported Venues

- **Curve** - Deep liquidity for stETH/ETH
- **Balancer** - Flash loans (0% fee) + swaps
- **Uniswap V3** - Multiple fee tiers

## Quick Start

### 1. Deploy Contract

```bash
# Set environment
export PRIVATE_KEY=your_key
export ETH_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY
export ETHERSCAN_API_KEY=your_key

# Deploy
cd scripts
chmod +x deploy.sh
./deploy.sh
```

### 2. Configure Bot

```bash
cd bot

# Copy environment template
cp .env.example .env

# Edit .env with your values:
# - PRIVATE_KEY
# - RPC_URL_PRIMARY
# - ARB_CONTRACT (from deployment)
```

### 3. Run Bot

```bash
# Development
cargo run

# Production (optimized)
cargo run --release
```

## Configuration

Edit `config.toml`:

```toml
[strategy]
min_spread_bps = 20        # 0.20% minimum spread
min_profit_wei = "10000000000000000"  # 0.01 ETH minimum
max_trade_size_eth = 10.0  # Max 10 ETH per trade
poll_interval_ms = 200     # 5 checks per second

[execution]
use_flashbots = true       # MEV protection
max_gas_price_gwei = 100   # Don't overpay for gas
```

## How It Works

### Flash Loan Arbitrage Flow

```
1. Detect price discrepancy:
   - stETH @ 0.997 ETH on Curve
   - stETH @ 1.002 ETH on Uniswap
   - Spread: 0.5%

2. Execute atomic arbitrage:
   ┌─────────────────────────────────────────┐
   │ Flash Loan 10 ETH from Balancer (0% fee)│
   │              │                          │
   │              ▼                          │
   │ Buy 10.03 stETH on Curve @ 0.997        │
   │              │                          │
   │              ▼                          │
   │ Sell 10.03 stETH on Uniswap @ 1.002     │
   │              │                          │
   │              ▼                          │
   │ Receive 10.05 ETH                       │
   │              │                          │
   │              ▼                          │
   │ Repay 10 ETH to Balancer                │
   │              │                          │
   │              ▼                          │
   │ Profit: 0.05 ETH (~$175)                │
   └─────────────────────────────────────────┘

3. If profit < minProfit: TX REVERTS (you only pay gas)
```

### MEV Protection

The bot uses **Flashbots Protect** by default:
- Transactions are private until mined
- No front-running risk
- Failed transactions are not broadcast

For thin spreads (<0.3%), consider using Flashbots bundles for maximum protection.

## File Structure

```
lst-arb/
├── contracts/
│   ├── src/
│   │   ├── LstArbitrage.sol      # Main contract
│   │   └── interfaces/           # DEX interfaces
│   └── foundry.toml
│
├── bot/
│   ├── src/
│   │   ├── main.rs               # Entry point
│   │   ├── config.rs             # Configuration
│   │   ├── rpc/                  # RPC load balancing
│   │   ├── price/                # Multicall quoter
│   │   ├── detector/             # Opportunity detection
│   │   ├── simulator/            # TX simulation
│   │   ├── executor/             # TX execution
│   │   └── monitor/              # Stats & alerts
│   ├── Cargo.toml
│   └── config.toml
│
└── scripts/
    └── deploy.sh
```

## Capital Requirements

| Item | Cost |
|------|------|
| Contract Deployment | ~$50-100 |
| Gas for first trades | ~$50-100 |
| **Total** | **~$100-200** |

Flash loans cover all trading capital!

## Expected Returns

| Market Condition | Opportunities/Day | Avg Profit | Daily Revenue |
|-----------------|-------------------|------------|---------------|
| Low volatility | 5-10 | $20-50 | $100-500 |
| Medium volatility | 10-30 | $50-150 | $500-3000 |
| High volatility | 30-100 | $100-500 | $3000-10000+ |

*LRTs (weETH, ezETH) typically have larger spreads than established LSTs*

## Monitoring

### Telegram Alerts

Set up alerts for:
- Confirmed transactions (profit amount)
- Reverted transactions (debug info)
- System errors

```toml
[monitoring]
telegram_bot_token = "YOUR_BOT_TOKEN"
telegram_chat_id = "YOUR_CHAT_ID"
```

### Stats Summary

Logged every 5 minutes:
```
═══════════════════════════════════════════
📊 BOT STATISTICS
═══════════════════════════════════════════
Uptime:              2h 30m
Opportunities Found: 47
Simulations Passed:  23
TXs Submitted:       18
TXs Confirmed:       16
TXs Reverted:        2
Win Rate:            88.9%
Gross Profit:        0.42 ETH
Gas Spent:           0.08 ETH
Net Profit:          0.34 ETH
═══════════════════════════════════════════
```

## Troubleshooting

### "No opportunities found"

- Check RPC health: Are connections stable?
- Lower `min_spread_bps` temporarily to see if spreads exist
- LRTs have larger spreads than LSTs

### "Simulation failed"

- Contract may not be configured for that token
- Run `deploy.sh` to configure all venues
- Check contract has approval for tokens

### "TX reverted"

- Price moved during execution (slippage)
- Increase `min_profit` buffer
- Use Flashbots for better MEV protection

### "Gas price too high"

- Increase `max_gas_price_gwei` in config
- Or wait for lower gas periods

## Advanced: Adding New Tokens

1. Add token address to `config.toml`
2. Find best DEX pools for the token
3. Configure contract:
   ```bash
   cast send $CONTRACT "configureUniswapV3(address,address,uint24)" \
       $TOKEN_ADDRESS $POOL_ADDRESS $FEE_TIER \
       --rpc-url $RPC --private-key $KEY
   ```
4. Add to `enabled_tokens` in config

## Security Notes

- **Never commit your private key**
- Use a dedicated wallet for the bot
- Start with small trade sizes
- Monitor closely for first few days
- Contract has `onlyOwner` protection

## License

MIT
