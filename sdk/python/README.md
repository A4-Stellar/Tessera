# Tessera Python SDK

`tessera-sdk` is the asynchronous Python client for the Tessera Stellar RWA API. It targets Python 3.10 and newer and returns strict Pydantic v2 models from the live Rust service.

## Install

```bash
python -m pip install tessera-sdk
```

The distribution is `tessera-sdk`; the import package is `tessera_sdk`.

## Quick start

The client base URL points to the API server root. It adds `/v1` to every data request.

```python
import asyncio

from tessera_sdk import TesseraClient


async def main() -> None:
    async with TesseraClient("http://localhost:8080") as client:
        stats = await client.get_stats()
        assets = await client.get_assets(active=True, limit=100)
        asset = await client.get_asset(1)
        print(stats.total_assets, len(assets), asset.name)


asyncio.run(main())
```

Every endpoint method is asynchronous and must be awaited.

## API methods

| Method | Live endpoint | Return value |
| --- | --- | --- |
| `await get_stats()` | `GET /v1/stats` | `Stats` |
| `await get_events()` | `GET /v1/events` | `list[Event]` |
| `await get_assets(...)` | `GET /v1/assets` | `list[Asset]` or `list[SparseAsset]` |
| `await get_asset(asset_id, ...)` | `GET /v1/assets/{id}` | `Asset` or `SparseAsset` |
| `await get_asset_events(asset_id, include_diagnostics=True)` | `GET /v1/assets/{id}/events` | `AssetEvents` |
| `await get_holders(asset_id, offset=0, limit=50)` | `GET /v1/assets/{id}/holders` | `list[Holder]` |
| `await get_asset_compliance(asset_id)` | `GET /v1/assets/{id}/compliance` | `ComplianceSummary` |
| `await get_dividends(asset_id)` | `GET /v1/assets/{id}/dividends` | `list[Distribution]` |
| `await get_distribution(asset_id, distribution_id)` | `GET /v1/assets/{id}/distributions/{did}` | `Distribution` |
| `await get_address_holdings(address)` | `GET /v1/holders/{address}` | `list[AddressHolding]` |
| `await get_holders_compliance(address)` | `GET /v1/holders/{address}/compliance` | `list[AddressCompliance]` |
| `await get_address_compliance(address)` | `GET /v1/compliance/{address}` | `list[AddressCompliance]` |

`get_assets` accepts `asset_type`, `active`, `offset`, `limit`, and `fields`. The API caps list and holder limits at 100. Holder pages use offset pagination.

## Sparse assets

Full asset responses require every field defined by the Rust `Asset` model and reject unknown fields. A request using `fields` intentionally returns only selected keys, so the SDK returns the safe `SparseAsset` model and leaves unselected fields as `None`.

```python
asset = await client.get_asset(1, fields=["id", "name", "active"])
assets = await client.get_assets(fields="id,name,active")

print(asset.name)
print(asset.model_dump(exclude_unset=True))
```

`fields` accepts a comma-separated string or a sequence of field names. Empty selections are omitted, and only the documented asset keys are accepted by the strict model.

## Holder DataFrame

`get_holders_dataframe` is asynchronous. Await it. It requests every holder page at the API maximum of 100 rows and returns a pandas DataFrame with columns `address`, `balance`, and `share_percent`.

```python
frame = await client.get_holders_dataframe(1)
print(frame)
print(frame["balance"].dtype)
```

Balances remain strings and the `balance` column uses pandas' nullable string dtype. The original integer text is never converted through a fixed-width numeric type, so values larger than 64 bits are preserved.

## Errors

Non-success responses are raised as typed exceptions. JSON bodies with `error` and `message`, JSON detail bodies, plain-text bodies, and empty error responses are supported.

```python
from tessera_sdk import NotFoundError, RateLimitError

try:
    await client.get_asset(999999)
except NotFoundError as error:
    print(error.status_code, error.error_code, error.message)
except RateLimitError as error:
    print(error.retry_after)
```

The exception classes are `APIError`, `NotFoundError`, `RateLimitError`, `ConnectionError`, and `ResponseError`. `retry_after` is available when the server supplies a numeric or HTTP-date `Retry-After` header. Successful responses that are not valid JSON or do not match the live wire model raise `ResponseError`.

## Injected HTTP clients

Pass an existing `httpx.AsyncClient` to use its transport, headers, proxies, and TLS configuration. Tessera does not close an injected client; the application that created it remains responsible for its lifecycle.

```python
import httpx

from tessera_sdk import TesseraClient

http_client = httpx.AsyncClient()
try:
    async with TesseraClient("http://localhost:8080", client=http_client) as client:
        await client.get_stats()
finally:
    await http_client.aclose()
```

A client created by `TesseraClient` is owned by the SDK and is closed by `await client.aclose()` or by `async with`. `TesseraClient.owns_http_client` and `TesseraClient.is_closed` expose the lifecycle state.

## Models and typing

All public response models are available from `tessera_sdk`. Models inherit strict validation from `TesseraModel`: unknown response fields are rejected, numeric and boolean values are not coerced, and Rust `u32`/`u64` bounds are enforced. Monetary and token integer strings remain strings. The package includes `py.typed` for static type checkers.

## Development

```bash
cd sdk/python
python -m venv .venv
. .venv/bin/activate
python -m pip install -e ".[test]"
ruff format --check .
ruff check .
mypy
pytest
```

Build and inspect the PyPI artifacts with:

```bash
python -m pip install build
python -m build
python -m twine check dist/*
```
