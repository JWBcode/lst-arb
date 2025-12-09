#!/usr/bin/env python3
"""Quick 0x API test - run this locally to verify your API key works"""

import requests

ZEROX_API_KEY = "c09b957e-9f63-4147-9f20-1fcf992eeb6c"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WSTETH = "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0"

print("Testing 0x API...")
print(f"API Key: {ZEROX_API_KEY[:8]}...{ZEROX_API_KEY[-4:]}")

try:
    resp = requests.get(
        "https://api.0x.org/swap/v1/price",
        params={
            "sellToken": WETH,
            "buyToken": WSTETH,
            "sellAmount": "1000000000000000000",  # 1 ETH
        },
        headers={"0x-api-key": ZEROX_API_KEY},
        timeout=10
    )

    print(f"\nStatus: {resp.status_code}")

    if resp.status_code == 200:
        data = resp.json()
        buy_amount = int(data.get("buyAmount", 0)) / 1e18
        print(f"SUCCESS! 1 WETH -> {buy_amount:.6f} wstETH")
        print(f"Price: {data.get('price', 'N/A')}")
        print(f"Gas: {data.get('estimatedGas', 'N/A')}")

        sources = [s['name'] for s in data.get('sources', []) if float(s.get('proportion', 0)) > 0]
        print(f"Sources: {', '.join(sources) if sources else 'N/A'}")
    else:
        print(f"Error: {resp.text[:200]}")

except Exception as e:
    print(f"Exception: {e}")
