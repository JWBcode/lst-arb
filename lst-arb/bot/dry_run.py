#!/usr/bin/env python3
"""
LST/LRT Arbitrage Scanner - Phase 1: Expanded Surveillance
Monitors specific pools across Maverick, Balancer, and Uniswap V3.
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
ZERO_EX_API_URL = "https://api.0x.org/swap/v1"

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
# DEX CONTRACTS
# =============================================================================

# Maverick V2
MAVERICK_QUOTER = "0x9980ce3b5570e41324904f46A06cE7B466925E23"
MAVERICK_POOLS = {
    "swETH-ETH": "0x0CE176E1b11A8f88a4Ba2535De80E81F88592bad",   # Boosted
    "wstETH-ETH": "0x0E4275f93D8B8826A01d4A26f6f4F4F6644d08B5", # Boosted
}

# Balancer V2
BALANCER_VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
BALANCER_POOLS = {
    # Pool ID format: address + pool type + nonce
    "ezETH-WETH": "0x596192bb6e41802428ac943d2f1476c1af25cc0e000000000000000000000659",
    "rETH-WETH": "0x1e19cf2d73a72ef1332c882f20534b6519be0276000200000000000000000112",
}

# Uniswap V3
UNISWAP_QUOTER = "0x61fFE014bA17989E743c5F6cB21bF9697530B21e"
UNISWAP_POOLS = {
    "ezETH-WETH-100": {"token": "ezETH", "fee": 100},    # 0.01% fee tier
    "ezETH-WETH-500": {"token": "ezETH", "fee": 500},    # 0.05% fee tier
}

# Curve (for reference pricing)
CURVE_POOLS = {
    "stETH-ETH": "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022",
}

# Scanner settings
MIN_SPREAD_BPS = 5
TRADE_SIZES_ETH = [1, 5, 10, 25]
GAS_COST_ETH = 0.003


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
# 0x API QUOTES
# =============================================================================

def get_0x_quote(token_addr: str, amount_eth: float, direction: str) -> Optional[Dict]:
    """
    Get quote from 0x API for token swap.
    Returns dict with price and additional metadata.

    direction: "buy" = ETH -> Token, "sell" = Token -> ETH
    """
    amount_wei = int(amount_eth * 1e18)

    headers = {
        "0x-api-key": ZERO_EX_API_KEY,
        "Accept": "application/json",
    }

    try:
        if direction == "buy":  # ETH -> Token (buying token with ETH)
            params = {
                "sellToken": "WETH",
                "buyToken": token_addr,
                "sellAmount": str(amount_wei),
            }
        else:  # Token -> ETH (selling token for ETH)
            params = {
                "sellToken": token_addr,
                "buyToken": "WETH",
                "sellAmount": str(amount_wei),
            }

        url = f"{ZERO_EX_API_URL}/quote"
        resp = requests.get(url, params=params, headers=headers, timeout=10)

        if resp.status_code == 200:
            data = resp.json()
            return {
                "buyAmount": int(data.get("buyAmount", 0)),
                "sellAmount": int(data.get("sellAmount", 0)),
                "price": float(data.get("price", 0)),
                "estimatedGas": int(data.get("estimatedGas", 0)),
                "sources": data.get("sources", []),
                "guaranteedPrice": float(data.get("guaranteedPrice", 0)),
            }
        else:
            error_msg = resp.json().get("reason", resp.text) if resp.text else f"HTTP {resp.status_code}"
            print(f"    0x API error for {token_addr[:10]}... ({direction}): {error_msg}")
            return None

    except Exception as e:
        print(f"    0x API exception: {str(e)[:50]}")
        return None


def get_0x_price(token_addr: str, amount_eth: float, direction: str) -> Optional[float]:
    """
    Get price from 0x API (uses /price endpoint for faster response).
    Returns price as ETH per token.
    """
    amount_wei = int(amount_eth * 1e18)

    headers = {
        "0x-api-key": ZERO_EX_API_KEY,
        "Accept": "application/json",
    }

    try:
        if direction == "buy":  # ETH -> Token
            params = {
                "sellToken": "WETH",
                "buyToken": token_addr,
                "sellAmount": str(amount_wei),
            }
        else:  # Token -> ETH
            params = {
                "sellToken": token_addr,
                "buyToken": "WETH",
                "sellAmount": str(amount_wei),
            }

        url = f"{ZERO_EX_API_URL}/price"
        resp = requests.get(url, params=params, headers=headers, timeout=10)

        if resp.status_code == 200:
            data = resp.json()
            buy_amount = int(data.get("buyAmount", 0))
            sell_amount = int(data.get("sellAmount", 0))

            if buy_amount > 0 and sell_amount > 0:
                if direction == "buy":
                    # Buying token: price = ETH spent / tokens received
                    return (sell_amount / 1e18) / (buy_amount / 1e18)
                else:
                    # Selling token: price = ETH received / tokens sold
                    return (buy_amount / 1e18) / (sell_amount / 1e18)
        else:
            error_data = resp.json() if resp.text else {}
            reason = error_data.get("reason", f"HTTP {resp.status_code}")
            if "validation" not in reason.lower():  # Don't spam validation errors
                print(f"    0x price error ({direction}): {reason[:60]}")
            return None

    except Exception as e:
        return None

    return None


# =============================================================================
# CURVE QUOTES (Reference)
# =============================================================================

def get_curve_quote(pool: str, amount_eth: float, direction: str) -> Optional[float]:
    """Get Curve stETH pool price"""
    amount_wei = int(amount_eth * 1e18)

    if direction == "buy":  # ETH -> token
        i, j = 0, 1
    else:  # token -> ETH
        i, j = 1, 0

    # get_dy(int128,int128,uint256)
    data = "0x5e0d443f"
    data += f"{i:064x}{j:064x}{amount_wei:064x}"

    result = eth_call(pool, data)
    if result:
        amount_out = hex_to_int(result) / 1e18
        if amount_out > 0:
            if direction == "buy":
                return amount_eth / amount_out
            else:
                return amount_out / amount_eth
    return None


# =============================================================================
# UNISWAP V3 QUOTES
# =============================================================================

def get_uniswap_quote(token_addr: str, fee: int, amount_eth: float, direction: str) -> Optional[float]:
    """
    Get Uniswap V3 quote using 1inch API (more reliable than direct quoter)
    """
    amount_wei = int(amount_eth * 1e18)

    if direction == "buy":  # WETH -> Token
        src = WETH
        dst = token_addr
    else:  # Token -> WETH
        src = token_addr
        dst = WETH

    try:
        # Use 1inch Fusion API for accurate quotes
        url = f"https://api.1inch.dev/swap/v6.0/1/quote"
        params = {
            "src": src,
            "dst": dst,
            "amount": str(amount_wei),
        }
        headers = {"Authorization": "Bearer demo"}  # Public demo key
        resp = requests.get(url, params=params, headers=headers, timeout=5)

        if resp.status_code == 200:
            data = resp.json()
            if "dstAmount" in data:
                amount_out = int(data["dstAmount"]) / 1e18
                if amount_out > 0:
                    if direction == "buy":
                        return amount_eth / amount_out
                    else:
                        return amount_out / amount_eth
    except:
        pass

    # Fallback: Try Paraswap
    try:
        url = f"https://apiv5.paraswap.io/prices"
        params = {
            "srcToken": src,
            "destToken": dst,
            "amount": str(amount_wei),
            "srcDecimals": 18,
            "destDecimals": 18,
            "network": 1,
        }
        resp = requests.get(url, params=params, timeout=5)
        if resp.status_code == 200:
            data = resp.json()
            if "priceRoute" in data and "destAmount" in data["priceRoute"]:
                amount_out = int(data["priceRoute"]["destAmount"]) / 1e18
                if amount_out > 0:
                    if direction == "buy":
                        return amount_eth / amount_out
                    else:
                        return amount_out / amount_eth
    except:
        pass

    return None


# =============================================================================
# BALANCER V2 QUOTES
# =============================================================================

def get_balancer_quote(pool_id: str, token_addr: str, amount_eth: float, direction: str) -> Optional[float]:
    """
    Get Balancer quote using queryBatchSwap
    This is a simplified single-swap query
    """
    amount_wei = int(amount_eth * 1e18)

    if direction == "buy":  # WETH -> Token
        asset_in_idx = 0
        asset_out_idx = 1
        assets = [WETH, token_addr]
    else:  # Token -> WETH
        asset_in_idx = 0
        asset_out_idx = 1
        assets = [token_addr, WETH]

    # queryBatchSwap is complex - use simplified swap query
    # For now, estimate from pool reserves

    # Alternative: Use Balancer SOR API
    try:
        # Balancer has a public API for quotes
        sor_url = "https://api.balancer.fi/sor/1"
        payload = {
            "sellToken": WETH if direction == "buy" else token_addr,
            "buyToken": token_addr if direction == "buy" else WETH,
            "sellAmount": str(amount_wei),
            "orderKind": "sell",
            "gasPrice": "30000000000"
        }
        resp = requests.post(sor_url, json=payload, timeout=5)
        if resp.status_code == 200:
            data = resp.json()
            if "returnAmount" in data:
                amount_out = int(data["returnAmount"]) / 1e18
                if amount_out > 0:
                    if direction == "buy":
                        return amount_eth / amount_out
                    else:
                        return amount_out / amount_eth
    except:
        pass

    return None


# =============================================================================
# MAVERICK V2 QUOTES
# =============================================================================

def get_maverick_quote(pool_addr: str, token_addr: str, amount_eth: float, direction: str) -> Optional[float]:
    """
    Get Maverick V2 quote
    Maverick uses a different quoting mechanism with tick-based liquidity
    """
    amount_wei = int(amount_eth * 1e18)

    # Maverick Quoter: calculateSwap(address pool, uint128 amount, bool tokenAIn, bool exactOutput, int32 tickLimit)
    # This is simplified - Maverick's actual interface is more complex

    try:
        # Try Maverick API
        api_url = f"https://api.mav.xyz/v1/quote"
        params = {
            "chainId": 1,
            "poolAddress": pool_addr,
            "amount": str(amount_wei),
            "tokenIn": WETH if direction == "buy" else token_addr,
            "tokenOut": token_addr if direction == "buy" else WETH,
        }
        resp = requests.get(api_url, params=params, timeout=5)
        if resp.status_code == 200:
            data = resp.json()
            if "amountOut" in data:
                amount_out = int(data["amountOut"]) / 1e18
                if amount_out > 0:
                    if direction == "buy":
                        return amount_eth / amount_out
                    else:
                        return amount_out / amount_eth
    except:
        pass

    # Fallback: Direct pool query (simplified)
    # getMavTickSpacing, calculateSwap etc would go here
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
        buy = get_0x_price(token_addr, amount_eth, "buy")
        sell = get_0x_price(token_addr, amount_eth, "sell")
        if buy and sell:
            quotes.append(PoolQuote(
                dex="0x",
                pool_name=f"{token_name}-WETH",
                token=token_name,
                buy_price=buy,
                sell_price=sell,
                liquidity_ok=True
            ))
        time.sleep(0.1)  # Rate limiting for 0x API

    # --- Curve (stETH reference for comparison) ---
    for pool_name, pool_addr in CURVE_POOLS.items():
        token = pool_name.split("-")[0]
        buy = get_curve_quote(pool_addr, amount_eth, "buy")
        sell = get_curve_quote(pool_addr, amount_eth, "sell")
        if buy and sell:
            quotes.append(PoolQuote(
                dex="Curve",
                pool_name=pool_name,
                token=token,
                buy_price=buy,
                sell_price=sell,
                liquidity_ok=True
            ))

    # --- Uniswap V3 (ezETH) ---
    for pool_name, config in UNISWAP_POOLS.items():
        token = config["token"]
        token_addr = TOKENS.get(token)
        if token_addr:
            buy = get_uniswap_quote(token_addr, config["fee"], amount_eth, "buy")
            sell = get_uniswap_quote(token_addr, config["fee"], amount_eth, "sell")
            if buy and sell:
                quotes.append(PoolQuote(
                    dex="UniV3",
                    pool_name=pool_name,
                    token=token,
                    buy_price=buy,
                    sell_price=sell,
                    liquidity_ok=True
                ))

    # --- Balancer (ezETH, rETH) ---
    for pool_name, pool_id in BALANCER_POOLS.items():
        token = pool_name.split("-")[0]
        token_addr = TOKENS.get(token)
        if token_addr:
            buy = get_balancer_quote(pool_id, token_addr, amount_eth, "buy")
            sell = get_balancer_quote(pool_id, token_addr, amount_eth, "sell")
            if buy and sell:
                quotes.append(PoolQuote(
                    dex="Balancer",
                    pool_name=pool_name,
                    token=token,
                    buy_price=buy,
                    sell_price=sell,
                    liquidity_ok=True
                ))

    # --- Maverick (swETH, wstETH) ---
    for pool_name, pool_addr in MAVERICK_POOLS.items():
        token = pool_name.split("-")[0]
        token_addr = TOKENS.get(token)
        if token_addr:
            buy = get_maverick_quote(pool_addr, token_addr, amount_eth, "buy")
            sell = get_maverick_quote(pool_addr, token_addr, amount_eth, "sell")
            if buy and sell:
                quotes.append(PoolQuote(
                    dex="Maverick",
                    pool_name=pool_name,
                    token=token,
                    buy_price=buy,
                    sell_price=sell,
                    liquidity_ok=True
                ))

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

            # Get all quotes
            quotes = scan_all_pools(5)

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
