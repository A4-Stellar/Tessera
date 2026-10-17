# tessera-pandas

Pandas helpers on top of `tessera-sdk`. Balances are returned as Arrow
`decimal256(39, 0)` columns, so 128-bit token amounts stay exact instead of
being rounded to `float64`.

```python
import asyncio

from tessera_pandas import TesseraPandasClient


async def main() -> None:
    async with TesseraPandasClient("http://localhost:8080") as client:
        holders = await client.get_holders_df(1)
        transfers = await client.get_transfers_df(1)
        print(holders.head(), transfers.head())


asyncio.run(main())
```

| Method | Source | Columns |
| --- | --- | --- |
| `await get_holders_df(asset_id)` | `GET /v1/assets/{id}/holders` (all pages) | `address`, `balance`, `share_percent` |
| `await get_transfers_df(asset_id)` | `GET /v1/events`, filtered to the asset's token | `id`, `ledger`, `timestamp`, `from`, `to`, `amount` |

`get_transfers_df` reads the indexer's recent-events feed, so it covers the
events the API currently retains, and `from`/`to` are `None` when the event
data does not include them. A runnable notebook is in
`examples/holders_and_transfers.ipynb`.

## Development

```bash
python -m pip install -e ../ -e ".[test]"
pytest
```
