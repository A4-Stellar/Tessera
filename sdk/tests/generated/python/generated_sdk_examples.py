import pytest
from tessera_sdk import TesseraClient

API_BASE = "http://localhost:8080"
client = TesseraClient(API_BASE)

class TestSDKPythonExamples:
    async def test_python_example_1_all(self):
        from tessera_sdk import TesseraClient
        client = TesseraClient("http://localhost:8080")
        async def get_assets():
            assets = await client.get_assets()
            for asset in assets:
                print(f"{asset.name}: {asset.total_supply} supply")
            return assets

    async def test_python_example_2_all(self):
        compliance = Contract("CBUERYDM…D2IU")
        op = compliance.call(
            "add_to_allowlist",
            Address(admin).toScVal(),
            Address(investor).toScVal(),
            nativeToScVal("US", { "type": "string" }),
            nativeToScVal(currentLedger + 6_300_000, { "type": "u32" }),
        )
