#!/usr/bin/env python3
"""
LST/LRT Arbitrage Scanner - Phase 1: Expanded Surveillance
Monitors LST/LRT tokens using 0x API for cross-DEX quotes.
"""

import json
import time
import requests
from datetime import datetime
from dataclasses import dataclass
from typing import Optional, Dict, List, Tuple

# =============================================================================
# CONFIGURATION
# =============================================================================

RPC_URL = "https://eth-mainnet.g.alchemy.com/v2/u_ybzLz2H0iPFztCKrLN1"

# 0x API Configuration
ZERO_EX_API_KEY = "c09b957e-9f63-4147-9f20-1fcf992eeb6c"
ZERO_EX_API_URL = "https://api.0x.org/swap/v1"  # Main endpoint for Ethereum mainnet

# User-Agent to bypass Cloudflare bot detection
USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36"

# Tokens
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
ETH_ADDRESS = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE"  # Native ETH placeholder
TOKENS = {
    "swETH": "0xf951E335afb289353dc249e82926178EaC7DEd78",
    "wstETH": "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0",
    "ezETH": "0xbf5495Efe5DB9ce00f80364C8B423567e58d2110",
    "rETH": "0xae78736Cd615f374D3085123A210448E74Fc6393",
    "stETH": "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84",
    "weETH": "0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee",
    "cbETH": "0xBe9895146f7AF43049ca1c1AE358B0541Ea49704",
    "rsETH": "0xA1290d69c65A6Fe4DF752f95823fae25cB99e5A7",
}

# =============================================================================
# SCANNER SETTINGS
# =============================================================================

MIN_SPREAD_BPS = 5
TRADE_SIZES_ETH = [1, 5, 10, 25]
GAS_COST_ETH = 0.003

# DEX sources to check (0x aggregates these)
DEX_SOURCES = ["Uniswap_V3", "Balancer_V2", "Curve", "Maverick_V2", "SushiSwap"]


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
# RPC HELPERS
# =============================================================================

def eth_call(to: str, data: str) -> Optional[str]:
    """Make eth_call to RPC"""
    try:
        resp = requests.post(RPC_URL, json={
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{"to": to, "data": data}, "latest"],
            "id": 1
        }, timeout=10)
        result = resp.json().get("result")
        if result and result != "0x":
            return result
    except Exception as e:
        pass
    return None


def hex_to_int(hex_str: str) -> int:
    """Convert hex string to int"""
    if hex_str.startswith("0x"):
        hex_str = hex_str[2:]
    return int(hex_str, 16) if hex_str else 0


# =============================================================================
# 0x API QUOTES (CORRECTED)
# =============================================================================

def get_0x_price(token_addr: str, amount_eth: float, direction: str) -> Optional[float]:
    """
    Get normalized price (ETH per Token) from 0x API.

    direction:
      "buy"  = ETH -> Token (Ask Price - we sell ETH to buy token)
      "sell" = Token -> ETH (Bid Price - we sell token to get ETH)

    Returns float price in ETH per token, or None if failure.
    """
    # Use main 0x API endpoint for Ethereum mainnet
    base_url = f"{ZERO_EX_API_URL}/price"

    headers = {
        "0x-api-key": ZERO_EX_API_KEY,
        "Accept": "application/json",
        # CRITICAL: User-Agent prevents Cloudflare Exit Code 56 Connection Reset
        "User-Agent": USER_AGENT,
    }

    try:
        if direction == "buy":
            # We are Selling ETH to Buy Token
            params = {
                "sellToken": "WETH",
                "buyToken": token_addr,
                "sellAmount": str(int(amount_eth * 1e18)),  # Amount of ETH we give
            }
        else:
            # We are Selling Token to Buy ETH
            # Use same ETH amount as proxy for token value query
            params = {
                "sellToken": token_addr,
                "buyToken": "WETH",
                "sellAmount": str(int(amount_eth * 1e18)),  # Proxy amount
            }

        response = requests.get(base_url, params=params, headers=headers, timeout=10)

        if response.status_code != 200:
            error_text = response.text[:100] if response.text else f"HTTP {response.status_code}"
            print(f"    [!] 0x API Error {response.status_code}: {error_text}")
            return None

        data = response.json()

        # Calculate implied price (ETH per Token)
        buy_amt = float(data.get("buyAmount", 0))
        sell_amt = float(data.get("sellAmount", 0))

        if buy_amt <= 0 or sell_amt <= 0:
            return None

        if direction == "buy":
            # We sold ETH (sell_amt), got Token (buy_amt)
            # Price = ETH spent / Tokens received
            return sell_amt / buy_amt
        else:
            # We sold Token (sell_amt), got ETH (buy_amt)
            # Price = ETH received / Tokens sold
            return buy_amt / sell_amt

    except requests.exceptions.RequestException as e:
        print(f"    [!] 0x Request Failed: {str(e)[:60]}")
        return None
    except Exception as e:
        print(f"    [!] 0x Parse Error: {str(e)[:60]}")
        return None


# =============================================================================
# CURVE QUOTES (Reference)
# =============================================================================

def get_0x_headers() -> dict:
    """Get headers for 0x API requests"""
    return {
        "0x-api-key": ZEROX_API_KEY,
        "Accept": "application/json",
    }


def get_0x_price(sell_token: str, buy_token: str, sell_amount_wei: int) -> Optional[dict]:
    """
    Get price quote from 0x API
    Returns full response including price, sources, and gas estimate
    """
    try:
        url = f"{ZEROX_API_URL}/price"
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
    Get quote for a token using 0x API
    Returns: (price_eth_per_token, primary_source, gas_estimate_eth)
    """
    amount_wei = int(amount_eth * 1e18)

    if direction == "buy":  # ETH -> Token (selling ETH, buying token)
        sell_token = WETH
        buy_token = token_addr
    else:  # Token -> ETH (selling token, buying ETH)
        sell_token = token_addr
        buy_token = WETH

    data = get_0x_price(sell_token, buy_token, amount_wei)

    if data and "buyAmount" in data:
        buy_amount = int(data["buyAmount"]) / 1e18

        if buy_amount > 0:
            # Calculate price (ETH per token)
            if direction == "buy":
                price = amount_eth / buy_amount  # ETH spent / tokens received
            else:
                price = buy_amount / amount_eth  # ETH received / tokens sold (normalized)

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

def scan_all_pools(amount_eth: float = 5) -> List[PoolQuote]:
    """Scan all configured pools for prices using 0x API"""
    quotes = []

    # --- 0x API (All tokens) ---
    print("\n  Fetching 0x API quotes for all tokens...")
    for token_name, token_addr in TOKENS.items():
        print(f"    Scanning {token_name}...", end="", flush=True)

        buy_price = get_0x_price(token_addr, amount_eth, "buy")
        sell_price = get_0x_price(token_addr, amount_eth, "sell")

        if buy_price and sell_price:
            quotes.append(PoolQuote(
                dex="0x",
                pool_name=f"{token_name}-WETH",
                token=token_name,
                buy_price=buy_price,
                sell_price=sell_price,
                liquidity_ok=True
            ))
            print(f" Buy: {buy_price:.6f} | Sell: {sell_price:.6f} ETH")
        else:
            print(" FAILED")

        time.sleep(0.2)  # Rate limiting for 0x API

    # --- Curve (stETH reference for comparison) ---
    for pool_name, pool_addr in CURVE_POOLS.items():
        token = pool_name.split("-")[0]
        buy = get_curve_quote(pool_addr, amount_eth, "buy")
        sell = get_curve_quote(pool_addr, amount_eth, "sell")
        if buy and sell:
            quotes.append(PoolQuote(
                dex=f"0x({buy_source})",
                pool_name=f"{token_name}-WETH",
                token=token_name,
                buy_price=buy_price,
                sell_price=sell_price,
                liquidity_ok=True
            ))
            print(f"    {token_name}: buy={buy_price:.6f} via {buy_source}, sell={sell_price:.6f} via {sell_source}")
        else:
            print(f"    {token_name}: No quotes available")

        # Small delay to avoid rate limiting
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
    print("  LST/LRT ARBITRAGE SCANNER - PHASE 1: 0x API INTEGRATION")
    print("=" * 75)
    print("  Data Sources:")
    print("    0x API:    ALL LST/LRT tokens (aggregated DEX quotes)")
    print("    Maverick:  ETH-swETH, ETH-wstETH (Boosted)")
    print("    Balancer:  ezETH-WETH, rETH-WETH (Weighted)")
    print("    UniswapV3: ezETH-WETH (0.01%, 0.05% tiers)")
    print("    Curve:     stETH-ETH (reference)")
    print("=" * 75)
    print("  Tokens via 0x API:")
    print(f"    {', '.join(TOKENS.keys())}")
    print("=" * 75)
    print(f"  Min Spread: {MIN_SPREAD_BPS} bps | Gas Est: {GAS_COST_ETH} ETH")
    print("=" * 75 + "\n")


def print_quotes(quotes: List[PoolQuote]):
    print("\n  POOL QUOTES:")
    print("  " + "-" * 70)
    print(f"  {'DEX':<10} {'Pool':<20} {'Token':<8} {'Buy':<12} {'Sell':<12} {'Spread':<10}")
    print("  " + "-" * 70)

    for q in quotes:
        spread = (q.sell_price - q.buy_price) / q.buy_price * 10000
        print(f"  {q.dex:<10} {q.pool_name:<20} {q.token:<8} {q.buy_price:<12.6f} {q.sell_price:<12.6f} {spread:>+8.1f} bps")


def print_opportunities(opps: List[Opportunity]):
    if not opps:
        print("\n  No arbitrage opportunities found.")
        return

    print(f"\n  OPPORTUNITIES FOUND: {len(opps)}")
    print("  " + "-" * 70)

    for i, opp in enumerate(opps[:5], 1):
        color = "\033[92m" if opp.net_profit_eth > 0 else "\033[93m"
        reset = "\033[0m"

        print(f"\n{color}  [{i}] {opp.token}: {opp.buy_dex}/{opp.buy_pool} → {opp.sell_dex}/{opp.sell_pool}{reset}")
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
                print("\n  No quotes retrieved. Check RPC connection.")

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
