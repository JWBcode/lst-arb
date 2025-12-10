#!/usr/bin/env python3
"""
LST/LRT Arbitrage Scanner - Arbitrum One with Waterfall Depth Check
Monitors LST/LRT tokens using 0x API for cross-DEX quotes.
Implements waterfall liquidity depth checking to map Arbitrum liquidity.
"""

import time
import requests
from datetime import datetime
from dataclasses import dataclass, field
from typing import Optional, Dict, List, Tuple

# =============================================================================
# CONFIGURATION - Arbitrum One
# =============================================================================

RPC_URL = "https://arb1.arbitrum.io/rpc"

# 0x API Configuration - Arbitrum (v2 API)
ZERO_EX_API_KEY = "c09b957e-9f63-4147-9f20-1fcf992eeb6c"
ZERO_EX_API_URL = "https://api.0x.org/swap/permit2"  # v2 API for Arbitrum
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
# SCANNER SETTINGS - Waterfall Depth Check
# =============================================================================

MIN_SPREAD_BPS = 5
SKIP_SPREAD_BPS = -50  # -0.5% spread threshold to skip token
GAS_COST_ETH = 0.0001  # Arbitrum L2 gas is much cheaper

# Waterfall depth levels (in ETH)
INITIAL_CHECK_ETH = 1.0  # First pass: check viability
DEPTH_LEVELS_ETH = [5.0, 10.0, 25.0]  # Deeper liquidity mapping


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
    trade_size_eth: float = 1.0


@dataclass
class LiquidityDepth:
    """Tracks liquidity depth at various trade sizes"""
    token: str
    viable: bool  # Passed initial check (spread >= -0.5%)
    initial_spread_bps: float
    depth_map: Dict[float, float] = field(default_factory=dict)  # size_eth -> spread_bps
    max_profitable_size: float = 0.0
    best_spread_bps: float = 0.0


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
    depth_info: Optional[LiquidityDepth] = None


# =============================================================================
# 0x API FUNCTIONS
# =============================================================================

def get_0x_headers() -> dict:
    """Get headers for 0x API v2 requests"""
    return {
        "0x-api-key": ZERO_EX_API_KEY,
        "0x-version": "v2",  # Required for v2 API
        "Accept": "application/json",
        "User-Agent": USER_AGENT,
    }


def get_0x_quote(sell_token: str, buy_token: str, sell_amount_wei: int) -> Optional[dict]:
    """
    Get price quote from 0x API v2 for Arbitrum.
    Returns full response including price, sources, and gas estimate.
    """
    try:
        url = f"{ZERO_EX_API_URL}/price"
        params = {
            "sellToken": sell_token,
            "buyToken": buy_token,
            "sellAmount": str(sell_amount_wei),
            "chainId": CHAIN_ID,  # Required for Arbitrum One
        }
        resp = requests.get(url, params=params, headers=get_0x_headers(), timeout=10)

        if resp.status_code == 200:
            data = resp.json()
            # v2 API returns liquidityAvailable flag
            if not data.get("liquidityAvailable", False):
                return None  # No liquidity route found
            return data
        elif resp.status_code == 429:
            print(f"    Rate limited, waiting...")
            time.sleep(2)
            return None
        else:
            # Only show error for non-rate-limit issues
            if resp.status_code != 400:  # 400 often means no route found
                print(f"    0x API error: {resp.status_code}")
    except requests.exceptions.Timeout:
        print(f"    0x API timeout")
    except Exception as e:
        print(f"    0x API exception: {type(e).__name__}")

    return None


def get_token_quote(token_name: str, token_addr: str, amount_eth: float, direction: str) -> Optional[Tuple[float, str, float]]:
    """
    Get quote for a token using 0x API v2 on Arbitrum.

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

            # Get primary liquidity source from v2 route
            primary_source = "0x"
            route = data.get("route", {})
            fills = route.get("fills", [])
            if fills:
                primary_source = fills[0].get("source", "0x")

            # Gas estimate from v2 response
            gas_price = int(data.get("gasPrice", "10000000"))  # v2 returns as string
            gas_estimate = int(data.get("gas", "200000"))  # v2 uses "gas" not "estimatedGas"
            gas_cost_eth = (gas_price * gas_estimate) / 1e18

            return (price, primary_source, gas_cost_eth)

    return None


# =============================================================================
# WATERFALL DEPTH CHECK
# =============================================================================

def calculate_spread_bps(buy_price: float, sell_price: float) -> float:
    """Calculate spread in basis points"""
    if buy_price <= 0:
        return -10000  # Invalid
    return ((sell_price - buy_price) / buy_price) * 10000


def check_token_viability(token_name: str, token_addr: str, check_amount: float = 1.0) -> Tuple[bool, float, Optional[Tuple[float, float, str]]]:
    """
    Initial viability check for a token at small trade size.

    Returns:
        (viable, spread_bps, (buy_price, sell_price, source) or None)
        viable = True if spread >= SKIP_SPREAD_BPS (-0.5%)
    """
    buy_result = get_token_quote(token_name, token_addr, check_amount, "buy")
    time.sleep(0.15)  # Rate limiting
    sell_result = get_token_quote(token_name, token_addr, check_amount, "sell")

    if not buy_result or not sell_result:
        return False, -10000, None

    buy_price, buy_source, _ = buy_result
    sell_price, sell_source, _ = sell_result

    spread_bps = calculate_spread_bps(buy_price, sell_price)

    viable = spread_bps >= SKIP_SPREAD_BPS
    return viable, spread_bps, (buy_price, sell_price, buy_source)


def map_liquidity_depth(token_name: str, token_addr: str, depth_levels: List[float]) -> LiquidityDepth:
    """
    Map liquidity depth for a token at increasing trade sizes.
    Only called for tokens that pass the initial viability check.
    """
    depth = LiquidityDepth(
        token=token_name,
        viable=True,
        initial_spread_bps=0,
        depth_map={},
        max_profitable_size=0,
        best_spread_bps=-10000
    )

    for size_eth in depth_levels:
        buy_result = get_token_quote(token_name, token_addr, size_eth, "buy")
        time.sleep(0.15)
        sell_result = get_token_quote(token_name, token_addr, size_eth, "sell")

        if buy_result and sell_result:
            buy_price, _, _ = buy_result
            sell_price, _, _ = sell_result
            spread_bps = calculate_spread_bps(buy_price, sell_price)

            depth.depth_map[size_eth] = spread_bps

            # Track best spread and max profitable size
            if spread_bps > depth.best_spread_bps:
                depth.best_spread_bps = spread_bps

            if spread_bps > 0:
                depth.max_profitable_size = size_eth
        else:
            depth.depth_map[size_eth] = None  # No liquidity at this depth

        time.sleep(0.1)  # Rate limiting between depth checks

    return depth


def waterfall_scan_token(token_name: str, token_addr: str) -> Optional[Tuple[LiquidityDepth, List[PoolQuote]]]:
    """
    Waterfall depth check for a single token.

    1. Query at INITIAL_CHECK_ETH (1.0 ETH)
    2. If spread < SKIP_SPREAD_BPS (-0.5%), SKIP the token
    3. If viable, query deeper liquidity at DEPTH_LEVELS_ETH

    Returns: (LiquidityDepth, [PoolQuote]) or None if token should be skipped
    """
    # Step 1: Initial viability check
    viable, initial_spread, quote_data = check_token_viability(
        token_name, token_addr, INITIAL_CHECK_ETH
    )

    if not viable:
        return None  # Skip this token

    # Create depth tracker
    depth = LiquidityDepth(
        token=token_name,
        viable=True,
        initial_spread_bps=initial_spread,
        depth_map={INITIAL_CHECK_ETH: initial_spread},
        best_spread_bps=initial_spread,
        max_profitable_size=INITIAL_CHECK_ETH if initial_spread > 0 else 0
    )

    quotes = []

    if quote_data:
        buy_price, sell_price, source = quote_data
        quotes.append(PoolQuote(
            dex=f"0x({source})",
            pool_name=f"{token_name}-WETH",
            token=token_name,
            buy_price=buy_price,
            sell_price=sell_price,
            liquidity_ok=True,
            trade_size_eth=INITIAL_CHECK_ETH
        ))

    # Step 2: Deep liquidity mapping (only if initial check passes)
    if initial_spread >= MIN_SPREAD_BPS:
        print(f"      ↳ Mapping liquidity depth...", end=" ", flush=True)

        for size_eth in DEPTH_LEVELS_ETH:
            buy_result = get_token_quote(token_name, token_addr, size_eth, "buy")
            time.sleep(0.15)
            sell_result = get_token_quote(token_name, token_addr, size_eth, "sell")

            if buy_result and sell_result:
                buy_price, source, _ = buy_result
                sell_price, _, _ = sell_result
                spread_bps = calculate_spread_bps(buy_price, sell_price)

                depth.depth_map[size_eth] = spread_bps

                if spread_bps > depth.best_spread_bps:
                    depth.best_spread_bps = spread_bps

                if spread_bps > 0:
                    depth.max_profitable_size = size_eth

                quotes.append(PoolQuote(
                    dex=f"0x({source})",
                    pool_name=f"{token_name}-WETH",
                    token=token_name,
                    buy_price=buy_price,
                    sell_price=sell_price,
                    liquidity_ok=True,
                    trade_size_eth=size_eth
                ))

                print(f"{size_eth}ETH:{spread_bps:+.0f}bps", end=" ", flush=True)
            else:
                depth.depth_map[size_eth] = None
                print(f"{size_eth}ETH:N/A", end=" ", flush=True)

            time.sleep(0.1)

        print()  # Newline after depth map

    return depth, quotes


# =============================================================================
# MAIN SCANNER
# =============================================================================

def scan_all_tokens_waterfall() -> Tuple[Dict[str, LiquidityDepth], List[PoolQuote]]:
    """
    Scan all tokens using waterfall depth check.
    Returns depth info and quotes for viable tokens only.
    """
    all_depths = {}
    all_quotes = []
    skipped = []

    print("\n  WATERFALL LIQUIDITY SCAN")
    print("  " + "-" * 70)
    print(f"  Initial check: {INITIAL_CHECK_ETH} ETH | Skip threshold: {SKIP_SPREAD_BPS} bps")
    print(f"  Depth levels: {DEPTH_LEVELS_ETH} ETH")
    print("  " + "-" * 70)

    for token_name, token_addr in TOKENS.items():
        print(f"\n  {token_name}:", end=" ", flush=True)

        result = waterfall_scan_token(token_name, token_addr)

        if result is None:
            skipped.append(token_name)
            print(f"\033[91mSKIPPED (spread < {SKIP_SPREAD_BPS} bps)\033[0m")
        else:
            depth, quotes = result
            all_depths[token_name] = depth
            all_quotes.extend(quotes)

            # Color code based on profitability
            if depth.best_spread_bps >= MIN_SPREAD_BPS:
                color = "\033[92m"  # Green
                status = "PROFITABLE"
            elif depth.initial_spread_bps >= 0:
                color = "\033[93m"  # Yellow
                status = "VIABLE"
            else:
                color = "\033[0m"  # Default
                status = "LOW SPREAD"

            print(f"{color}{status}{color} | Initial: {depth.initial_spread_bps:+.1f} bps | Best: {depth.best_spread_bps:+.1f} bps\033[0m")

        time.sleep(0.2)  # Rate limiting between tokens

    if skipped:
        print(f"\n  \033[91mSkipped tokens (illiquid): {', '.join(skipped)}\033[0m")

    return all_depths, all_quotes


def find_arbitrage_with_depth(depths: Dict[str, LiquidityDepth], quotes: List[PoolQuote]) -> List[Opportunity]:
    """Find arbitrage opportunities considering liquidity depth"""
    opportunities = []

    # Group quotes by token and size
    by_token_size: Dict[str, Dict[float, PoolQuote]] = {}
    for q in quotes:
        if q.token not in by_token_size:
            by_token_size[q.token] = {}
        by_token_size[q.token][q.trade_size_eth] = q

    # Check each token's depth for profitable sizes
    for token, depth in depths.items():
        if token not in by_token_size:
            continue

        for size_eth, spread_bps in depth.depth_map.items():
            if spread_bps is None or spread_bps < MIN_SPREAD_BPS:
                continue

            if size_eth not in by_token_size[token]:
                continue

            q = by_token_size[token][size_eth]

            # Calculate profit
            tokens_bought = size_eth / q.buy_price
            eth_received = tokens_bought * q.sell_price
            gross = eth_received - size_eth
            net = gross - GAS_COST_ETH

            opportunities.append(Opportunity(
                token=token,
                buy_dex=q.dex,
                buy_pool=q.pool_name,
                sell_dex=q.dex,  # Same source via 0x
                sell_pool=q.pool_name,
                spread_bps=spread_bps,
                trade_size_eth=size_eth,
                gross_profit_eth=gross,
                net_profit_eth=net,
                depth_info=depth
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
    print(f"  Waterfall Check: {INITIAL_CHECK_ETH} ETH → {DEPTH_LEVELS_ETH} ETH")
    print(f"  Skip Threshold: {SKIP_SPREAD_BPS} bps | Min Spread: {MIN_SPREAD_BPS} bps")
    print(f"  Gas Est: {GAS_COST_ETH} ETH (L2)")
    print("=" * 75 + "\n")


def print_depth_summary(depths: Dict[str, LiquidityDepth]):
    print("\n  LIQUIDITY DEPTH SUMMARY:")
    print("  " + "-" * 70)
    print(f"  {'Token':<10} {'1 ETH':>10} {'5 ETH':>10} {'10 ETH':>10} {'25 ETH':>10} {'Max Size':>10}")
    print("  " + "-" * 70)

    for token, depth in depths.items():
        row = f"  {token:<10}"

        for size in [INITIAL_CHECK_ETH] + DEPTH_LEVELS_ETH:
            spread = depth.depth_map.get(size)
            if spread is None:
                row += f"{'N/A':>10}"
            elif spread >= MIN_SPREAD_BPS:
                row += f"\033[92m{spread:>+9.0f}bp\033[0m"
            elif spread >= 0:
                row += f"\033[93m{spread:>+9.0f}bp\033[0m"
            else:
                row += f"\033[91m{spread:>+9.0f}bp\033[0m"

        # Max profitable size
        if depth.max_profitable_size > 0:
            row += f"\033[92m{depth.max_profitable_size:>9.0f}E\033[0m"
        else:
            row += f"{'0':>10}"

        print(row)


def print_opportunities(opps: List[Opportunity]):
    if not opps:
        print("\n  No arbitrage opportunities found.")
        return

    print(f"\n  OPPORTUNITIES FOUND: {len(opps)}")
    print("  " + "-" * 70)

    for i, opp in enumerate(opps[:10], 1):
        color = "\033[92m" if opp.net_profit_eth > 0 else "\033[93m"
        reset = "\033[0m"

        print(f"\n{color}  [{i}] {opp.token} @ {opp.trade_size_eth:.1f} ETH{reset}")
        print(f"      Route: {opp.buy_dex} → {opp.sell_dex}")
        print(f"      Spread: {opp.spread_bps:.1f} bps")
        print(f"      Gross: {opp.gross_profit_eth:+.6f} ETH | Net: {opp.net_profit_eth:+.6f} ETH (${opp.net_profit_eth * 3100:+.2f})")

        if opp.depth_info:
            print(f"      Max profitable size: {opp.depth_info.max_profitable_size:.0f} ETH")


def run_scanner():
    print_header()

    stats = {"scans": 0, "opps": 0, "profitable": 0, "total_profit": 0.0}

    print("Starting scanner with Waterfall Depth Check... (Ctrl+C to stop)\n")

    try:
        while True:
            stats["scans"] += 1
            print(f"\n{'═' * 75}")
            print(f"  SCAN #{stats['scans']} | {datetime.now().strftime('%H:%M:%S')}")
            print(f"{'═' * 75}")

            # Waterfall scan all tokens
            depths, quotes = scan_all_tokens_waterfall()

            if depths:
                # Show liquidity depth summary
                print_depth_summary(depths)

                # Find opportunities with depth consideration
                opps = find_arbitrage_with_depth(depths, quotes)
                print_opportunities(opps)

                # Update stats
                for opp in opps[:5]:
                    stats["opps"] += 1
                    if opp.net_profit_eth > 0:
                        stats["profitable"] += 1
                        stats["total_profit"] += opp.net_profit_eth
            else:
                print("\n  No viable tokens found. All tokens skipped due to low liquidity.")

            print(f"\n{'─' * 75}")
            print(f"  SESSION: {stats['opps']} opps | {stats['profitable']} profitable | {stats['total_profit']:.4f} ETH")

            # Longer wait between full scans since we're doing deeper checks
            print(f"\n  Next scan in 10 seconds...")
            time.sleep(10)

    except KeyboardInterrupt:
        print("\n\n" + "=" * 75)
        print("  FINAL SUMMARY")
        print("=" * 75)
        print(f"  Scans: {stats['scans']} | Opportunities: {stats['opps']}")
        print(f"  Profitable: {stats['profitable']} | Theoretical: {stats['total_profit']:.4f} ETH")
        print("=" * 75 + "\n")


if __name__ == "__main__":
    run_scanner()
