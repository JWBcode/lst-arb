#!/usr/bin/env python3
"""
LST/LRT Arbitrage Scanner - Phase 1: 0x API Integration
Monitors LST/LRT tokens using 0x API for cross-DEX quotes.
"""

import time
import requests
from datetime import datetime
from dataclasses import dataclass
from typing import Optional, Dict, List, Tuple

# =============================================================================
# CONFIGURATION - Arbitrum One
# =============================================================================

RPC_URL = "https://arb1.arbitrum.io/rpc"

# 0x API Configuration - Arbitrum
ZERO_EX_API_KEY = "c09b957e-9f63-4147-9f20-1fcf992eeb6c"
ZERO_EX_API_URL = "https://arbitrum.api.0x.org/swap/v1"
CHAIN_ID = 42161

# User-Agent to bypass Cloudflare bot detection
USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36"

# Tokens - Arbitrum One addresses (stETH not available on L2)
WETH = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
TOKENS = {
    "wstETH": "0x5979D7b546E38E41137eFe97697CBca551Db098E",
    "rETH": "0xEC70Dcb4A1EfA46b8F2D97C310C9c4790bA5ffA8",
    "cbETH": "0x1DEBd73E752bEaF79865Fd6446b0c970EaE7732f",
    "weETH": "0x35751007a407ca6feffe80b3cb397736d2cf4dbe",
    "ezETH": "0x2416092f143378750bb29b79ed961ab195cceea5",
}

# =============================================================================
# SCANNER SETTINGS
# =============================================================================

MIN_SPREAD_BPS = 5
TRADE_SIZES_ETH = [0.1, 0.25, 0.5]  # Reduced for <$200 capital on L2
GAS_COST_ETH = 0.0001  # Arbitrum L2 gas is much cheaper


# =============================================================================
# DATA STRUCTURES
# =============================================================================

@dataclass
class PoolQuote:
    dex: str
    pool_name: str
    token: str
    buy_price: float   # ETH per token when buying token
    sell_price: float  # ETH per token when selling token
    liquidity_ok: bool


@dataclass
class Opportunity:
    token: str
    buy_dex: str
    buy_pool: str
    sell_dex: str
    sell_pool: str
    spread_bps: float
    trade_size_eth: float
    gross_profit_eth: float
    net_profit_eth: float


# =============================================================================
# 0x API FUNCTIONS
# =============================================================================

def get_0x_headers() -> dict:
    """Get headers for 0x API requests"""
    return {
        "0x-api-key": ZERO_EX_API_KEY,
        "Accept": "application/json",
        "User-Agent": USER_AGENT,
    }


def get_0x_quote(sell_token: str, buy_token: str, sell_amount_wei: int) -> Optional[dict]:
    """
    Get price quote from 0x API.
    Returns full response including price, sources, and gas estimate.
    """
    try:
        url = f"{ZERO_EX_API_URL}/price"
        params = {
            "sellToken": sell_token,
            "buyToken": buy_token,
            "sellAmount": str(sell_amount_wei),
        }
        resp = requests.get(url, params=params, headers=get_0x_headers(), timeout=10)

        if resp.status_code == 200:
            return resp.json()
        else:
            print(f"    0x API error: {resp.status_code} - {resp.text[:100]}")
    except Exception as e:
        print(f"    0x API exception: {e}")

    return None


def get_token_quote(token_name: str, token_addr: str, amount_eth: float, direction: str) -> Optional[Tuple[float, str, float]]:
    """
    Get quote for a token using 0x API.

    Args:
        token_name: Name of the token (for logging)
        token_addr: Token contract address
        amount_eth: Amount to trade in ETH
        direction: "buy" (ETH -> Token) or "sell" (Token -> ETH)

    Returns: (price_eth_per_token, primary_source, gas_estimate_eth) or None
    """
    amount_wei = int(amount_eth * 1e18)

    if direction == "buy":  # ETH -> Token (selling ETH, buying token)
        sell_token = WETH
        buy_token = token_addr
    else:  # Token -> ETH (selling token, buying ETH)
        sell_token = token_addr
        buy_token = WETH

    data = get_0x_quote(sell_token, buy_token, amount_wei)

    if data and "buyAmount" in data:
        buy_amount = int(data["buyAmount"]) / 1e18

        if buy_amount > 0:
            # Calculate price (ETH per token)
            if direction == "buy":
                price = amount_eth / buy_amount  # ETH spent / tokens received
            else:
                price = buy_amount / amount_eth  # ETH received / tokens sold

            # Get primary liquidity source
            sources = data.get("sources", [])
            primary_source = "0x"
            for src in sources:
                if float(src.get("proportion", 0)) > 0:
                    primary_source = src.get("name", "0x")
                    break

            # Gas estimate
            gas_price = int(data.get("gasPrice", 30e9))
            gas_estimate = int(data.get("estimatedGas", 200000))
            gas_cost_eth = (gas_price * gas_estimate) / 1e18

            return (price, primary_source, gas_cost_eth)

    return None


# =============================================================================
# MAIN SCANNER
# =============================================================================

def scan_all_tokens(amount_eth: float = 5) -> List[PoolQuote]:
    """Scan all configured tokens for prices using 0x API"""
    quotes = []

    print("\n  Fetching 0x API quotes for all tokens...")

    for token_name, token_addr in TOKENS.items():
        print(f"    {token_name}:", end=" ", flush=True)

        # Get buy quote (ETH -> Token)
        buy_result = get_token_quote(token_name, token_addr, amount_eth, "buy")

        # Get sell quote (Token -> ETH)
        sell_result = get_token_quote(token_name, token_addr, amount_eth, "sell")

        if buy_result and sell_result:
            buy_price, buy_source, _ = buy_result
            sell_price, sell_source, _ = sell_result

            quotes.append(PoolQuote(
                dex=f"0x({buy_source})",
                pool_name=f"{token_name}-WETH",
                token=token_name,
                buy_price=buy_price,
                sell_price=sell_price,
                liquidity_ok=True
            ))
            print(f"buy={buy_price:.6f} via {buy_source}, sell={sell_price:.6f} via {sell_source}")
        else:
            print("No quotes available")

        # Rate limiting
        time.sleep(0.2)

    return quotes


def find_arbitrage(quotes: List[PoolQuote], trade_size: float) -> List[Opportunity]:
    """Find arbitrage opportunities across pools"""
    opportunities = []

    # Group quotes by token
    by_token: Dict[str, List[PoolQuote]] = {}
    for q in quotes:
        if q.token not in by_token:
            by_token[q.token] = []
        by_token[q.token].append(q)

    # Check each token for cross-pool arb
    for token, token_quotes in by_token.items():
        if len(token_quotes) < 2:
            continue

        for buy_q in token_quotes:
            for sell_q in token_quotes:
                if buy_q.pool_name == sell_q.pool_name:
                    continue

                # Buy on buy_q, sell on sell_q
                # Profit if sell_price > buy_price
                if sell_q.sell_price > buy_q.buy_price:
                    spread_bps = ((sell_q.sell_price - buy_q.buy_price) / buy_q.buy_price) * 10000

                    if spread_bps >= MIN_SPREAD_BPS:
                        tokens_bought = trade_size / buy_q.buy_price
                        eth_received = tokens_bought * sell_q.sell_price
                        gross = eth_received - trade_size
                        net = gross - GAS_COST_ETH

                        opportunities.append(Opportunity(
                            token=token,
                            buy_dex=buy_q.dex,
                            buy_pool=buy_q.pool_name,
                            sell_dex=sell_q.dex,
                            sell_pool=sell_q.pool_name,
                            spread_bps=spread_bps,
                            trade_size_eth=trade_size,
                            gross_profit_eth=gross,
                            net_profit_eth=net
                        ))

    return sorted(opportunities, key=lambda x: x.net_profit_eth, reverse=True)


# =============================================================================
# DISPLAY
# =============================================================================

def print_header():
    print("\n" + "=" * 75)
    print("  LST/LRT ARBITRAGE SCANNER - ARBITRUM ONE")
    print("=" * 75)
    print("  Network: Arbitrum One (Chain ID: 42161)")
    print("  Data Source: 0x Protocol (DEX Aggregator)")
    print("  Aggregates: Uniswap V3, Balancer, Curve, Camelot, SushiSwap, etc.")
    print("=" * 75)
    print("  Tokens:")
    print(f"    {', '.join(TOKENS.keys())}")
    print("=" * 75)
    print(f"  Min Spread: {MIN_SPREAD_BPS} bps | Gas Est: {GAS_COST_ETH} ETH (L2)")
    print("=" * 75 + "\n")


def print_quotes(quotes: List[PoolQuote]):
    print("\n  POOL QUOTES:")
    print("  " + "-" * 70)
    print(f"  {'DEX':<18} {'Pool':<15} {'Token':<8} {'Buy':<12} {'Sell':<12} {'Spread':<10}")
    print("  " + "-" * 70)

    for q in quotes:
        spread = (q.sell_price - q.buy_price) / q.buy_price * 10000
        print(f"  {q.dex:<18} {q.pool_name:<15} {q.token:<8} {q.buy_price:<12.6f} {q.sell_price:<12.6f} {spread:>+8.1f} bps")


def print_opportunities(opps: List[Opportunity]):
    if not opps:
        print("\n  No arbitrage opportunities found.")
        return

    print(f"\n  OPPORTUNITIES FOUND: {len(opps)}")
    print("  " + "-" * 70)

    for i, opp in enumerate(opps[:5], 1):
        color = "\033[92m" if opp.net_profit_eth > 0 else "\033[93m"
        reset = "\033[0m"

        print(f"\n{color}  [{i}] {opp.token}: {opp.buy_dex}/{opp.buy_pool} -> {opp.sell_dex}/{opp.sell_pool}{reset}")
        print(f"      Spread: {opp.spread_bps:.1f} bps | Size: {opp.trade_size_eth} ETH")
        print(f"      Gross: {opp.gross_profit_eth:+.6f} ETH | Net: {opp.net_profit_eth:+.6f} ETH (${opp.net_profit_eth * 3100:+.2f})")


def run_scanner():
    print_header()

    stats = {"scans": 0, "opps": 0, "profitable": 0, "total_profit": 0.0}

    print("Starting scanner... (Ctrl+C to stop)\n")

    try:
        while True:
            stats["scans"] += 1
            print(f"\n{'─' * 75}")
            print(f"  SCAN #{stats['scans']} | {datetime.now().strftime('%H:%M:%S')}")
            print(f"{'─' * 75}")

            # Get all quotes via 0x API
            quotes = scan_all_tokens(5)

            if quotes:
                print_quotes(quotes)

                # Find opportunities across all trade sizes
                all_opps = []
                for size in TRADE_SIZES_ETH:
                    opps = find_arbitrage(quotes, size)
                    all_opps.extend(opps)

                # Sort and display
                all_opps = sorted(all_opps, key=lambda x: x.net_profit_eth, reverse=True)
                print_opportunities(all_opps)

                # Update stats
                for opp in all_opps[:5]:
                    stats["opps"] += 1
                    if opp.net_profit_eth > 0:
                        stats["profitable"] += 1
                        stats["total_profit"] += opp.net_profit_eth
            else:
                print("\n  No quotes retrieved. Check API connection.")

            print(f"\n{'─' * 75}")
            print(f"  SESSION: {stats['opps']} opps | {stats['profitable']} profitable | {stats['total_profit']:.4f} ETH")

            time.sleep(5)

    except KeyboardInterrupt:
        print("\n\n" + "=" * 75)
        print("  FINAL SUMMARY")
        print("=" * 75)
        print(f"  Scans: {stats['scans']} | Opportunities: {stats['opps']}")
        print(f"  Profitable: {stats['profitable']} | Theoretical: {stats['total_profit']:.4f} ETH")
        print("=" * 75 + "\n")


if __name__ == "__main__":
    run_scanner()
