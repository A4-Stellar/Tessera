import pytest
from tessera_sdk import TesseraClient

API_BASE = "http://localhost:8080"
client = TesseraClient(API_BASE)

class TestSDKPythonExamples:
  async def test_python_example_1_integration_mdx(self):
    # Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\integration.mdx:70
    from tessera_sdk import TesseraClient
    
    client = TesseraClient("http://localhost:8080")
    
    async def get_assets():
        assets = await client.get_assets()
        for asset in assets:
            print(f"{asset.name}: {asset.total_supply} supply")
        return assets

