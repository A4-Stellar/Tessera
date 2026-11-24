"""Python sandbox example — same client, sandbox tenant header."""

import asyncio
import os

from tessera_sdk import TesseraClient

API = os.getenv("TESSERA_SANDBOX_URL", "http://localhost:8081")
TENANT = os.getenv("X_SANDBOX_TENANT_ID", "acme-1")


async def main() -> None:
    async with TesseraClient(
        API, default_headers={"X-Sandbox-Tenant-ID": TENANT}
    ) as client:
        # Identical call shape to production.
        stats = await client.get_stats()
        assets = await client.list_assets(limit=10)
        print(f"tenant={TENANT} assets={stats.total_assets} first={assets[0].id}")

        # Sandbox-only lifecycle call.
        reset = await client.reset_sandbox()
        print(f"reset in {reset.duration_ms} ms")


if __name__ == "__main__":
    asyncio.run(main())
