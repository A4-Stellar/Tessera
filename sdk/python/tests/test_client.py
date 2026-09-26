from collections.abc import Callable
from typing import Any

import httpx
import pandas as pd
import pytest
from pydantic import ValidationError

from tessera_sdk import (
    AddressCompliance,
    AddressHolding,
    Asset,
    AssetEvents,
    ComplianceSummary,
    ConnectionError,
    Distribution,
    Event,
    Holder,
    NotFoundError,
    RateLimitError,
    ResponseError,
    SparseAsset,
    Stats,
    TesseraClient,
)

BASE_URL = "https://api.example.test"
ADDRESS = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA"
HUGE_BALANCE = "170141183460469231731687303715884105727"


def asset_payload() -> dict[str, Any]:
    return {
        "id": 1,
        "token_contract": "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ",
        "issuer": ADDRESS,
        "name": "Manhattan Loft",
        "symbol": "MLOFT",
        "asset_type": "real_estate",
        "description": "A tokenized loft",
        "valuation_cents": "500000000",
        "valuation_usd": 5000000.0,
        "decimals": 2,
        "total_supply": "1000000",
        "holders": 2,
        "active": True,
        "paused": False,
        "compliance_contract": (
            "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU"
        ),
        "created_at_ledger": 3502885,
        "indexed_at_ledger": 3502999,
        "index_error": None,
    }


def holder_payload(
    address: str = ADDRESS,
    balance: str = HUGE_BALANCE,
) -> dict[str, Any]:
    return {
        "address": address,
        "balance": balance,
        "share_percent": 100.0,
    }


def distribution_payload() -> dict[str, Any]:
    return {
        "id": 10,
        "asset_token": "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ",
        "payment_token": "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC",
        "total_amount": "100000000000",
        "distributed": "25000000000",
        "claimed_percent": 25.0,
        "overflow_detected": False,
        "completed": False,
        "created_at_ledger": 3510000,
        "fiat_equivalent_usd": None,
    }


def compliance_payload() -> dict[str, Any]:
    return {
        "total_records": 2,
        "approved": 2,
        "suspended": 0,
        "rejected": 0,
        "pending": 0,
        "with_expiry": 1,
        "jurisdictions": [{"jurisdiction": "US", "count": 2}],
    }


def address_compliance_payload() -> dict[str, Any]:
    return {
        "address": ADDRESS,
        "asset_id": 1,
        "asset_name": "Manhattan Loft",
        "symbol": "MLOFT",
        "balance": HUGE_BALANCE,
        "status": "approved",
        "allowed": True,
    }


def stats_payload() -> dict[str, Any]:
    return {
        "total_assets": 1,
        "active_assets": 1,
        "tvl_cents": "500000000",
        "tvl_usd": 5000000.0,
        "total_holders": 1,
        "total_distributions": 1,
        "last_indexed_ledger": 3514152,
        "last_updated": "2026-07-09T08:43:12.101772791+00:00",
    }


async def test_all_live_v1_endpoints_return_strict_models() -> None:
    requests: list[httpx.Request] = []
    payloads: dict[str, Any] = {
        "/v1/stats": stats_payload(),
        "/v1/events": [
            {
                "id": 7,
                "contract": "CCONTRACT",
                "event_type": "transfer",
                "ledger": 42,
                "timestamp": None,
                "data": {"amount": HUGE_BALANCE},
            }
        ],
        "/v1/assets": [asset_payload()],
        "/v1/assets/1": asset_payload(),
        "/v1/assets/1/events": {
            "asset_id": 1,
            "diagnostics": [
                {
                    "contract": "CCONTRACT",
                    "event_type": "lockup",
                    "topics": ["lockup"],
                    "data": "1000",
                    "in_successful_contract_call": False,
                    "error_code": 10,
                }
            ],
        },
        "/v1/assets/1/holders": [holder_payload()],
        "/v1/assets/1/compliance": compliance_payload(),
        "/v1/assets/1/dividends": [distribution_payload()],
        "/v1/assets/1/distributions/10": distribution_payload(),
        f"/v1/holders/{ADDRESS}": [
            {
                "address": ADDRESS,
                "asset_id": 1,
                "asset_name": "Manhattan Loft",
                "symbol": "MLOFT",
                "balance": HUGE_BALANCE,
                "share_percent": 100.0,
            }
        ],
        f"/v1/holders/{ADDRESS}/compliance": [address_compliance_payload()],
        f"/v1/compliance/{ADDRESS}": [address_compliance_payload()],
    }

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.url.path not in payloads:
            return httpx.Response(
                404, json={"error": "not_found", "message": "missing"}
            )
        return httpx.Response(200, json=payloads[request.url.path])

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    stats = await client.get_stats()
    events = await client.get_events()
    assets = await client.get_assets(
        asset_type="real_estate",
        active=True,
        offset=0,
        limit=100,
    )
    asset = await client.get_asset(1)
    asset_events = await client.get_asset_events(1, include_diagnostics=True)
    holders = await client.get_holders(1, offset=0, limit=100)
    compliance = await client.get_asset_compliance(1)
    dividends = await client.get_dividends(1)
    distribution = await client.get_distribution(1, 10)
    address_holdings = await client.get_address_holdings(ADDRESS)
    holder_compliance = await client.get_holders_compliance(ADDRESS)
    address_compliance = await client.get_address_compliance(ADDRESS)

    assert isinstance(stats, Stats)
    assert isinstance(events[0], Event)
    assert isinstance(assets[0], Asset)
    assert isinstance(asset, Asset)
    assert isinstance(asset_events, AssetEvents)
    assert isinstance(holders[0], Holder)
    assert isinstance(compliance, ComplianceSummary)
    assert isinstance(dividends[0], Distribution)
    assert isinstance(distribution, Distribution)
    assert isinstance(address_holdings[0], AddressHolding)
    assert isinstance(holder_compliance[0], AddressCompliance)
    assert isinstance(address_compliance[0], AddressCompliance)
    assert holders[0].balance == HUGE_BALANCE
    assert [request.url.path for request in requests] == list(payloads)
    assert requests[2].url.params["asset_type"] == "real_estate"
    assert requests[2].url.params["active"] == "true"
    assert requests[2].url.params["limit"] == "100"
    assert requests[4].url.params["include_diagnostics"] == "true"
    assert requests[5].url.params["offset"] == "0"
    assert requests[5].url.params["limit"] == "100"
    await http_client.aclose()


async def test_sparse_asset_requests_use_sparse_models() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        fields = request.url.params.get("fields", "").split(",")
        values = {"id": 1, "name": "Manhattan Loft", "active": True}
        if request.url.path == "/v1/assets":
            return httpx.Response(200, json=[{key: values[key] for key in fields}])
        return httpx.Response(200, json={key: values[key] for key in fields})

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    asset = await client.get_asset(1, fields=" id, name,active ")
    assets = await client.get_assets(fields=["id", "name", "active"])

    assert isinstance(asset, SparseAsset)
    assert isinstance(assets[0], SparseAsset)
    assert asset.model_dump(exclude_unset=True) == {
        "id": 1,
        "name": "Manhattan Loft",
        "active": True,
    }
    assert assets[0].model_dump(exclude_unset=True).keys() == {
        "id",
        "name",
        "active",
    }
    await http_client.aclose()


async def test_holders_dataframe_fetches_every_page_and_preserves_strings() -> None:
    offsets: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        offset = request.url.params.get("offset")
        assert offset is not None
        offsets.append(offset)
        assert request.url.params.get("limit") == "100"
        if offset == "0":
            page = [
                holder_payload(address=f"G{index}", balance=str(10**index))
                for index in range(100)
            ]
        else:
            page = [holder_payload(address="LAST", balance=HUGE_BALANCE)]
        return httpx.Response(200, json=page)

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    frame = await client.get_holders_dataframe(1)

    assert isinstance(frame, pd.DataFrame)
    assert len(frame) == 101
    assert offsets == ["0", "100"]
    assert list(frame.columns) == ["address", "balance", "share_percent"]
    assert str(frame["balance"].dtype) == "string"
    assert frame.iloc[-1]["balance"] == HUGE_BALANCE
    assert isinstance(frame.iloc[-1]["balance"], str)
    await http_client.aclose()


async def test_empty_holder_result_creates_typed_dataframe() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.params["offset"] == "0"
        return httpx.Response(200, json=[])

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    frame = await client.get_holders_dataframe(1)

    assert frame.empty
    assert list(frame.columns) == ["address", "balance", "share_percent"]
    assert str(frame["balance"].dtype) == "string"
    await http_client.aclose()


async def test_json_404_raises_typed_error() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            404,
            json={"error": "not_found", "message": "no asset with id 9"},
        )

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    with pytest.raises(NotFoundError) as caught:
        await client.get_asset(9)

    assert caught.value.status_code == 404
    assert caught.value.error_code == "not_found"
    assert caught.value.message == "no asset with id 9"
    assert caught.value.body == {
        "error": "not_found",
        "message": "no asset with id 9",
    }
    await http_client.aclose()


async def test_plain_text_429_raises_rate_limit_error_with_retry_after() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            429,
            text="Too Many Requests",
            headers={"Retry-After": "12"},
        )

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    with pytest.raises(RateLimitError) as caught:
        await client.get_stats()

    assert caught.value.status_code == 429
    assert caught.value.retry_after == 12.0
    assert caught.value.message == "Too Many Requests"
    assert caught.value.body == "Too Many Requests"
    await http_client.aclose()


async def test_empty_and_json_http_errors_are_robust() -> None:
    async def run(
        status: int,
        response_factory: Callable[[httpx.Request], httpx.Response],
    ) -> Exception:
        def handler(request: httpx.Request) -> httpx.Response:
            return response_factory(request)

        http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        client = TesseraClient(BASE_URL, client=http_client)
        try:
            await client.get_stats()
        except Exception as error:
            return error
        finally:
            await http_client.aclose()
        raise AssertionError("request should fail")

    empty_error = await run(503, lambda request: httpx.Response(503))
    assert isinstance(empty_error, Exception)
    assert "HTTP 503" in str(empty_error)

    detail_error = await run(
        400,
        lambda request: httpx.Response(400, json={"detail": "invalid offset"}),
    )
    assert "invalid offset" in str(detail_error)


async def test_transport_error_is_wrapped() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("network unavailable", request=request)

    http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = TesseraClient(BASE_URL, client=http_client)

    with pytest.raises(ConnectionError) as caught:
        await client.get_stats()

    assert caught.value.message == "network unavailable"
    assert caught.value.request is not None
    await http_client.aclose()


async def test_invalid_or_nonconforming_success_body_raises_response_error() -> None:
    responses = [
        httpx.Response(200, text="not json"),
        httpx.Response(200, json={"id": 1}),
    ]

    for response in responses:

        def handler(
            request: httpx.Request, value: httpx.Response = response
        ) -> httpx.Response:
            return value

        http_client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        client = TesseraClient(BASE_URL, client=http_client)
        with pytest.raises(ResponseError):
            await client.get_stats()
        await http_client.aclose()


def test_models_reject_missing_and_unknown_fields_and_coercion() -> None:
    payload = asset_payload()
    assert Asset.model_validate(payload).id == 1

    incomplete = dict(payload)
    del incomplete["name"]
    with pytest.raises(ValidationError):
        Asset.model_validate(incomplete)

    extra = {**payload, "unexpected": True}
    with pytest.raises(ValidationError):
        Asset.model_validate(extra)

    coerced = {**payload, "id": "1"}
    with pytest.raises(ValidationError):
        Asset.model_validate(coerced)


def test_models_keep_i128_strings_and_diagnostic_json_values() -> None:
    asset = Asset.model_validate(asset_payload())
    distribution = Distribution.model_validate(distribution_payload())
    asset_events = AssetEvents.model_validate(
        {
            "asset_id": 1,
            "diagnostics": [
                {
                    "contract": None,
                    "event_type": "unknown",
                    "topics": ["symbol", {"nested": [1, True, None]}],
                    "data": ["any", "json", "value"],
                    "in_successful_contract_call": True,
                    "error_code": None,
                }
            ],
        }
    )

    assert asset.valuation_cents == "500000000"
    assert asset.total_supply == "1000000"
    assert distribution.total_amount == "100000000000"
    assert distribution.distributed == "25000000000"
    assert asset_events.diagnostics[0].data == ["any", "json", "value"]


async def test_async_context_closes_only_owned_http_client() -> None:
    transport_client = httpx.AsyncClient(
        transport=httpx.MockTransport(lambda request: httpx.Response(200, json=[]))
    )
    async with TesseraClient(BASE_URL, client=transport_client) as client:
        assert client.owns_http_client is False
        assert client.http_client is transport_client
    assert not transport_client.is_closed
    await client.aclose()
    assert not transport_client.is_closed
    await transport_client.aclose()

    owned = TesseraClient(BASE_URL)
    assert owned.owns_http_client is True
    await owned.aclose()
    assert owned.is_closed
    await owned.aclose()


async def test_context_rejects_already_closed_client() -> None:
    transport_client = httpx.AsyncClient(
        transport=httpx.MockTransport(lambda request: httpx.Response(200, json=[]))
    )
    await transport_client.aclose()
    client = TesseraClient(BASE_URL, client=transport_client)

    with pytest.raises(RuntimeError, match="already closed"):
        async with client:
            pass
