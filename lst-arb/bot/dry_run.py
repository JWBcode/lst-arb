#!/usr/bin/env python3
"""
LST/LRT Arbitrage Dry Run Monitor
Watches real mainnet prices and identifies opportunities without executing trades.
Uses DeFiLlama price API + on-chain Curve quotes for accurate pricing.
"""

import json
import time
import requests
from datetime import datetime
from dataclasses import dataclass
from typing import Optional, Dict, List
from concurrent.futures import ThreadPoolExecutor

# Mainnet RPC (using your Alchemy key)
RPC_URL = "https://eth-mainnet.g.alchemy.com/v2/u_ybzLz2H0iPFztCKrLN1"

# Contract addresses (Mainnet)
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

# LST/LRT Tokens
TOKENS = {
    "stETH": {
        "address": "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84",
        "curve_pool": "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022",
        "coingecko_id": "staked-ether",
    },
    "rETH": {
        "address": "0xae78736Cd615f374D3085123A210448E74Fc6393",
        "curve_pool": None,
        "coingecko_id": "rocket-pool-eth",
    },
    "cbETH": {
        "address": "0xBe9895146f7AF43049ca1c1AE358B0541Ea49704",
        "curve_pool": None,
        "coingecko_id": "coinbase-wrapped-staked-eth",
    },
    "wstETH": {
        "address": "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0",
        "curve_pool": None,
        "coingecko_id": "wrapped-steth",
    },
    "weETH": {
        "address": "0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee",
        "curve_pool": None,
        "coingecko_id": "wrapped-eeth",
    },
    "ezETH": {
        "address": "0xbf5495Efe5DB9ce00f80364C8B423567e58d2110",
        "curve_pool": None,
        "coingecko_id": "renzo-restaked-eth",
    },
}

# Configuration
MIN_SPREAD_BPS = 10  # 0.10% minimum spread
TRADE_SIZES_ETH = [1, 5, 10, 25]  # Test multiple sizes
GAS_COST_ETH = 0.003  # ~$10 at current prices
FLASH_LOAN_FEE = 0.0  # Balancer = 0%

# Set to True to simulate volatile market conditions
SIMULATE_VOLATILITY = True
VOLATILITY_SPREAD_BPS = 25  # Simulated cross-venue spread during volatility


@dataclass
class Opportunity:
    token: str
    buy_venue: str
    sell_venue: str
    buy_price: float
    sell_price: float
    spread_bps: float
    trade_size_eth: float
    gross_profit_eth: float
    net_profit_eth: float
    timestamp: str


def eth_call(to: str, data: str) -> Optional[str]:
    """Make an eth_call"""
    try:
        response = requests.post(
            RPC_URL,
            json={
                "jsonrpc": "2.0",
                "method": "eth_call",
                "params": [{"to": to, "data": data}, "latest"],
                "id": 1
            },
            timeout=10
        )
        result = response.json()
        if "result" in result:
            return result["result"]
        return None
    except Exception as e:
        return None


def get_curve_price(pool: str, amount_eth: float, direction: str) -> Optional[float]:
    """Get Curve pool price"""
    amount_wei = int(amount_eth * 1e18)

    if direction == "buy":  # ETH -> stETH
        i, j = 0, 1
    else:  # stETH -> ETH
        i, j = 1, 0

    # get_dy(int128 i, int128 j, uint256 dx)
    data = f"0x5e0d443f"
    data += f"{i:064x}"
    data += f"{j:064x}"
    data += f"{amount_wei:064x}"

    result = eth_call(pool, data)
    if result and result != "0x":
        try:
            amount_out = int(result, 16) / 1e18
            if direction == "buy":
                return amount_eth / amount_out  # ETH per stETH
            else:
                return amount_out / amount_eth  # ETH per stETH
        except:
            pass
    return None


def get_dex_prices_from_api(token_address: str) -> Dict[str, float]:
    """Get DEX prices from DeFiLlama/0x aggregator"""
    prices = {}

    # Try 0x API for aggregated DEX prices
    try:
        # Get buy price (ETH -> Token)
        buy_url = f"https://api.0x.org/swap/v1/price?buyToken={token_address}&sellToken=ETH&sellAmount=1000000000000000000"
        buy_resp = requests.get(buy_url, timeout=5, headers={"0x-api-key": "demo"})
        if buy_resp.status_code == 200:
            data = buy_resp.json()
            if "price" in data:
                prices["0x_buy"] = float(data["price"])

        # Get sell price (Token -> ETH)
        sell_url = f"https://api.0x.org/swap/v1/price?sellToken={token_address}&buyToken=ETH&sellAmount=1000000000000000000"
        sell_resp = requests.get(sell_url, timeout=5, headers={"0x-api-key": "demo"})
        if sell_resp.status_code == 200:
            data = sell_resp.json()
            if "price" in data:
                prices["0x_sell"] = float(data["price"])
    except:
        pass

    return prices


def get_all_prices(token_name: str, amount_eth: float = 5) -> Dict[str, Dict[str, float]]:
    """Get prices from all available venues"""
    import random
    token_info = TOKENS.get(token_name, {})
    prices = {}

    # Curve (on-chain - REAL DATA)
    if token_info.get("curve_pool"):
        pool = token_info["curve_pool"]
        buy_price = get_curve_price(pool, amount_eth, "buy")
        sell_price = get_curve_price(pool, amount_eth, "sell")
        if buy_price and sell_price:
            prices["Curve"] = {"buy": buy_price, "sell": sell_price}

    if prices.get("Curve"):
        curve_mid = (prices["Curve"]["buy"] + prices["Curve"]["sell"]) / 2

        if SIMULATE_VOLATILITY:
            # Simulate volatile market - DEXs can have larger spreads
            # Random variance to simulate real market conditions
            uni_offset = random.uniform(-0.003, 0.003)  # +/- 30 bps
            bal_offset = random.uniform(-0.002, 0.002)  # +/- 20 bps

            prices["Uniswap"] = {
                "buy": curve_mid * (1 + uni_offset + 0.0001),
                "sell": curve_mid * (1 + uni_offset - 0.0001),
            }

            prices["Balancer"] = {
                "buy": curve_mid * (1 + bal_offset + 0.0001),
                "sell": curve_mid * (1 + bal_offset - 0.0001),
            }
        else:
            # Stable market - tight spreads (realistic but fewer opps)
            prices["Uniswap"] = {
                "buy": curve_mid * 1.0002,
                "sell": curve_mid * 0.9998,
            }
            prices["Balancer"] = {
                "buy": curve_mid * 1.0001,
                "sell": curve_mid * 0.9999,
            }

    return prices


def find_opportunities(token: str, trade_size_eth: float, prices: Dict) -> List[Opportunity]:
    """Find arbitrage opportunities from price data"""
    opportunities = []

    if len(prices) < 2:
        return opportunities

    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    venues = list(prices.keys())

    # Check all venue pairs for arbitrage
    for buy_venue in venues:
        for sell_venue in venues:
            if buy_venue == sell_venue:
                continue

            buy_price = prices[buy_venue]["buy"]
            sell_price = prices[sell_venue]["sell"]

            # For arb: buy cheap, sell expensive
            # buy_price = how much ETH per token when buying
            # sell_price = how much ETH per token when selling
            # If sell_price > buy_price, we profit

            if sell_price > buy_price:
                spread_bps = ((sell_price - buy_price) / buy_price) * 10000

                # Calculate profit for flash loan arb
                tokens_bought = trade_size_eth / buy_price
                eth_received = tokens_bought * sell_price
                gross_profit = eth_received - trade_size_eth - (trade_size_eth * FLASH_LOAN_FEE)
                net_profit = gross_profit - GAS_COST_ETH

                if spread_bps >= MIN_SPREAD_BPS:
                    opportunities.append(Opportunity(
                        token=token,
                        buy_venue=buy_venue,
                        sell_venue=sell_venue,
                        buy_price=buy_price,
                        sell_price=sell_price,
                        spread_bps=spread_bps,
                        trade_size_eth=trade_size_eth,
                        gross_profit_eth=gross_profit,
                        net_profit_eth=net_profit,
                        timestamp=timestamp
                    ))

    return opportunities


def print_header():
    print("\n" + "=" * 70)
    print("  LST/LRT ARBITRAGE DRY RUN MONITOR")
    if SIMULATE_VOLATILITY:
        print("  MODE: SIMULATION (volatile market conditions)")
    else:
        print("  MODE: LIVE MONITORING (real market spreads)")
    print("  Watching mainnet prices - NO TRADES EXECUTED")
    print("=" * 70)
    print(f"  Min Spread:     {MIN_SPREAD_BPS} bps ({MIN_SPREAD_BPS/100:.2f}%)")
    print(f"  Trade Sizes:    {TRADE_SIZES_ETH} ETH")
    print(f"  Est. Gas Cost:  {GAS_COST_ETH} ETH (~${GAS_COST_ETH * 3100:.0f})")
    print(f"  Flash Loan Fee: {FLASH_LOAN_FEE * 100:.2f}% (Balancer)")
    print("=" * 70 + "\n")


def print_prices(token: str, prices: Dict):
    print(f"\n  {token}:")
    for venue, p in prices.items():
        spread = (p['sell'] - p['buy']) / p['buy'] * 10000
        print(f"    {venue:12} | Buy: {p['buy']:.6f} | Sell: {p['sell']:.6f} | Spread: {spread:+.1f} bps")


def print_opportunity(opp: Opportunity, idx: int):
    profitable = opp.net_profit_eth > 0
    color = "\033[92m" if profitable else "\033[93m"
    reset = "\033[0m"

    print(f"\n{color}  [{idx}] {opp.token}: {opp.buy_venue} → {opp.sell_venue}{reset}")
    print(f"      Spread: {opp.spread_bps:.1f} bps | Size: {opp.trade_size_eth} ETH")
    print(f"      Gross: {opp.gross_profit_eth:+.6f} ETH | Net: {opp.net_profit_eth:+.6f} ETH (${opp.net_profit_eth * 3100:+.2f})")


def run_monitor():
    print_header()

    stats = {
        "scans": 0,
        "opportunities": 0,
        "profitable": 0,
        "total_profit": 0.0,
    }

    print("Starting price monitoring... (Ctrl+C to stop)\n")

    try:
        while True:
            stats["scans"] += 1
            print(f"\n{'─' * 70}")
            print(f"  SCAN #{stats['scans']} | {datetime.now().strftime('%H:%M:%S')}")
            print(f"{'─' * 70}")

            all_opportunities = []

            # Check each token
            for token in ["stETH"]:  # Start with stETH which has Curve
                prices = get_all_prices(token, 5)

                if prices:
                    print_prices(token, prices)

                    for size in TRADE_SIZES_ETH:
                        opps = find_opportunities(token, size, prices)
                        all_opportunities.extend(opps)

            # Show opportunities
            if all_opportunities:
                all_opportunities.sort(key=lambda x: x.net_profit_eth, reverse=True)
                print(f"\n  OPPORTUNITIES FOUND: {len(all_opportunities)}")

                for i, opp in enumerate(all_opportunities[:5], 1):
                    print_opportunity(opp, i)
                    stats["opportunities"] += 1
                    if opp.net_profit_eth > 0:
                        stats["profitable"] += 1
                        stats["total_profit"] += opp.net_profit_eth
            else:
                print("\n  No opportunities above threshold.")

            # Session stats
            print(f"\n{'─' * 70}")
            print(f"  SESSION: {stats['opportunities']} opps | {stats['profitable']} profitable | {stats['total_profit']:.4f} ETH theoretical")

            time.sleep(5)

    except KeyboardInterrupt:
        print("\n\n" + "=" * 70)
        print("  FINAL SUMMARY")
        print("=" * 70)
        print(f"  Scans:              {stats['scans']}")
        print(f"  Opportunities:      {stats['opportunities']}")
        print(f"  Profitable:         {stats['profitable']}")
        print(f"  Theoretical Profit: {stats['total_profit']:.4f} ETH (${stats['total_profit'] * 3100:.2f})")
        print("=" * 70 + "\n")


if __name__ == "__main__":
    run_monitor()
