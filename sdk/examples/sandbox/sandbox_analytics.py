"""Python-analytics sandbox example.

The analytics library has no HTTP client of its own, so this example reads the
sandbox's synthetic holder distribution with `tessera_sdk` and feeds it into
the same analytics functions used in production.
"""

import asyncio
import os

from tessera_analytics import calculate_gini_coefficient, holder_retention
from tessera_sdk import TesseraClient

API = os.getenv("TESSERA_SANDBOX_URL", "http://localhost:8081")
TENANT = os.getenv("X_SANDBOX_TENANT_ID", "acme-1")
ASSET_ID = int(os.getenv("SANDBOX_ASSET_ID", "1"))


async def main() -> None:
    async with TesseraClient(
        API, default_headers={"X-Sandbox-Tenant-ID": TENANT}
    ) as client:
        holders = await client.list_holders(ASSET_ID, limit=1000)

    balances = [float(h.balance) for h in holders]
    print("holders:", len(balances))
    print("gini:", round(calculate_gini_coefficient(balances), 4))
    print("retention:", holder_retention(balances))


if __name__ == "__main__":
    asyncio.run(main())
