#!/usr/bin/env python3
"""
LST/LRT Arbitrage Bot Dashboard
Real-time monitoring interface
"""

from flask import Flask, render_template, jsonify
from flask_cors import CORS
import json
import time
import threading
import requests
from datetime import datetime
from pathlib import Path

app = Flask(__name__)
CORS(app)

# Configuration - Arbitrum One
CONFIG_PATH = Path(__file__).parent.parent / "bot" / "config.toml"
RPC_URL = "https://arb1.arbitrum.io/rpc"
CHAIN_ID = 42161
CHAIN_NAME = "Arbitrum One"
WALLET_ADDRESS = "0x7d3aed887225446aa3c398a50ddbb62be12d918a"
CONTRACT_ADDRESS = "0x0000000000000000000000000000000000000000"  # Update after deployment

# In-memory state (would be populated by actual bot)
bot_state = {
    "status": "idle",
    "uptime_start": None,
    "opportunities_found": 0,
    "simulations_passed": 0,
    "txs_submitted": 0,
    "txs_confirmed": 0,
    "txs_reverted": 0,
    "gross_profit_eth": 0.0,
    "gas_spent_eth": 0.0,
    "last_opportunity": None,
    "recent_trades": [],
    "price_feeds": {},
    "rpc_status": {
        "primary": {"status": "unknown", "latency_ms": 0},
        "backup1": {"status": "unknown", "latency_ms": 0},
        "backup2": {"status": "disconnected", "latency_ms": 0},
    }
}


def eth_rpc_call(method, params=[]):
    """Make an Ethereum RPC call"""
    try:
        response = requests.post(
            RPC_URL,
            json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1},
            timeout=10
        )
        return response.json().get("result")
    except Exception as e:
        print(f"RPC Error: {e}")
        return None


def get_wallet_balance():
    """Get wallet ETH balance"""
    result = eth_rpc_call("eth_getBalance", [WALLET_ADDRESS, "latest"])
    if result:
        return int(result, 16) / 1e18
    return 0


def get_gas_price():
    """Get current gas price in gwei"""
    result = eth_rpc_call("eth_gasPrice")
    if result:
        return int(result, 16) / 1e9
    return 0


def get_block_number():
    """Get current block number"""
    result = eth_rpc_call("eth_blockNumber")
    if result:
        return int(result, 16)
    return 0


def check_rpc_health():
    """Check RPC endpoint health"""
    start = time.time()
    result = eth_rpc_call("eth_blockNumber")
    latency = int((time.time() - start) * 1000)

    if result:
        bot_state["rpc_status"]["primary"] = {
            "status": "connected",
            "latency_ms": latency
        }
    else:
        bot_state["rpc_status"]["primary"] = {
            "status": "error",
            "latency_ms": 0
        }


def background_updater():
    """Background thread to update state"""
    while True:
        try:
            check_rpc_health()
        except Exception as e:
            print(f"Background update error: {e}")
        time.sleep(5)


# Start background updater
threading.Thread(target=background_updater, daemon=True).start()


@app.route("/")
def index():
    return render_template("index.html")


@app.route("/api/status")
def api_status():
    """Get bot status"""
    uptime = 0
    if bot_state["uptime_start"]:
        uptime = int(time.time() - bot_state["uptime_start"])

    win_rate = 0
    total_txs = bot_state["txs_confirmed"] + bot_state["txs_reverted"]
    if total_txs > 0:
        win_rate = (bot_state["txs_confirmed"] / total_txs) * 100

    return jsonify({
        "status": bot_state["status"],
        "uptime_seconds": uptime,
        "opportunities_found": bot_state["opportunities_found"],
        "simulations_passed": bot_state["simulations_passed"],
        "txs_submitted": bot_state["txs_submitted"],
        "txs_confirmed": bot_state["txs_confirmed"],
        "txs_reverted": bot_state["txs_reverted"],
        "win_rate": round(win_rate, 1),
        "gross_profit_eth": bot_state["gross_profit_eth"],
        "gas_spent_eth": bot_state["gas_spent_eth"],
        "net_profit_eth": bot_state["gross_profit_eth"] - bot_state["gas_spent_eth"],
    })


@app.route("/api/wallet")
def api_wallet():
    """Get wallet info"""
    balance = get_wallet_balance()
    return jsonify({
        "address": WALLET_ADDRESS,
        "balance_eth": round(balance, 6),
        "contract_address": CONTRACT_ADDRESS,
    })


@app.route("/api/network")
def api_network():
    """Get network info"""
    gas_price = get_gas_price()
    block = get_block_number()
    return jsonify({
        "chain": CHAIN_NAME,
        "chain_id": CHAIN_ID,
        "block_number": block,
        "gas_price_gwei": round(gas_price, 2),
        "rpc_status": bot_state["rpc_status"],
    })


@app.route("/api/trades")
def api_trades():
    """Get recent trades"""
    # Demo data for UI testing - Arbitrum tokens
    demo_trades = [
        {
            "timestamp": "2024-01-15 14:32:01",
            "token": "wstETH",
            "buy_venue": "Curve",
            "sell_venue": "Uniswap",
            "amount_eth": 0.5,
            "profit_eth": 0.0025,
            "status": "confirmed",
            "tx_hash": "0xabc...123"
        },
        {
            "timestamp": "2024-01-15 14:28:45",
            "token": "weETH",
            "buy_venue": "Balancer",
            "sell_venue": "Uniswap",
            "amount_eth": 0.3,
            "profit_eth": 0.0018,
            "status": "confirmed",
            "tx_hash": "0xdef...456"
        },
        {
            "timestamp": "2024-01-15 14:25:12",
            "token": "ezETH",
            "buy_venue": "Uniswap",
            "sell_venue": "Balancer",
            "amount_eth": 0.25,
            "profit_eth": -0.0003,
            "status": "reverted",
            "tx_hash": "0xghi...789"
        },
    ]
    return jsonify(bot_state.get("recent_trades", demo_trades))


@app.route("/api/opportunities")
def api_opportunities():
    """Get current opportunities being monitored"""
    # Demo data showing spread opportunities - Arbitrum tokens
    opportunities = [
        {
            "token": "wstETH",
            "curve_price": 0.9985,
            "uniswap_price": 1.0012,
            "balancer_price": 0.9998,
            "best_spread_bps": 27,
            "profitable": True
        },
        {
            "token": "weETH",
            "curve_price": None,
            "uniswap_price": 1.0045,
            "balancer_price": 1.0008,
            "best_spread_bps": 37,
            "profitable": True
        },
        {
            "token": "rETH",
            "curve_price": None,
            "uniswap_price": 1.0901,
            "balancer_price": 1.0895,
            "best_spread_bps": 6,
            "profitable": False
        },
        {
            "token": "ezETH",
            "curve_price": None,
            "uniswap_price": 0.9956,
            "balancer_price": 0.9934,
            "best_spread_bps": 22,
            "profitable": True
        },
    ]
    return jsonify(opportunities)


@app.route("/api/start", methods=["POST"])
def api_start():
    """Start the bot"""
    bot_state["status"] = "running"
    bot_state["uptime_start"] = time.time()
    return jsonify({"success": True, "status": "running"})


@app.route("/api/stop", methods=["POST"])
def api_stop():
    """Stop the bot"""
    bot_state["status"] = "stopped"
    return jsonify({"success": True, "status": "stopped"})


if __name__ == "__main__":
    print("\n" + "="*50)
    print("  LST/LRT Arbitrage Bot Dashboard")
    print("  Network: Arbitrum One (Chain ID: 42161)")
    print("="*50)
    print(f"\n  Dashboard: http://localhost:5000")
    print(f"  Wallet:    {WALLET_ADDRESS[:10]}...{WALLET_ADDRESS[-8:]}")
    print(f"  Contract:  {CONTRACT_ADDRESS[:10]}...{CONTRACT_ADDRESS[-8:]}")
    print("\n" + "="*50 + "\n")

    app.run(host="0.0.0.0", port=5000, debug=True)
