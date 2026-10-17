from typing import Any

import httpx
import pandas as pd
import pyarrow as pa

from tessera_pandas import I128_TYPE, TesseraPandasClient

BASE_URL = "https://api.example.test"
ADDRESS = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA"
TOKEN = "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ"
HUGE = "170141183460469231731687303715884105727"  # i128::MAX


def asset_payload() -> dict[str, Any]:
    return {
        "id": 1,
        "token_contract": TOKEN,
        "issuer": ADDRESS,
        "name": "Manhattan Loft",
        "symbol": "MLOFT",
        "asset_type": "real_estate",
        "description": "A tokenized loft",
        "valuation_cents": "500000000",
        "valuation_usd": 5000000.0,
        "decimals": 2,
        "total_supply": HUGE,
        "holders": 1,
        "active": True,
        "paused": False,
        "compliance_contract": (
            "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU"
        ),
        "created_at_ledger": 1,
        "indexed_at_ledger": 2,
        "index_error": None,
    }


def make_client(
    payloads: dict[str, Any],
) -> tuple[TesseraPandasClient, httpx.AsyncClient]:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=payloads[request.url.path])

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    return TesseraPandasClient(BASE_URL, client=http), http


async def test_holders_df_keeps_int128_balances_exact() -> None:
    client, http = make_client(
        {
            "/v1/assets/1/holders": [
                {"address": ADDRESS, "balance": HUGE, "share_percent": 100.0}
            ]
        }
    )

    frame = await client.get_holders_df(1)

    assert frame["balance"].dtype == pd.ArrowDtype(I128_TYPE)
    assert frame["balance"].iloc[0].as_py() == int(HUGE)
    await http.aclose()


async def test_empty_holders_df_is_typed() -> None:
    client, http = make_client({"/v1/assets/1/holders": []})

    frame = await client.get_holders_df(1)

    assert frame.empty
    assert frame["balance"].dtype == pd.ArrowDtype(pa.decimal256(39, 0))
    await http.aclose()


async def test_transfers_df_filters_by_asset_token_and_event_type() -> None:
    def event(id_: int, contract: str, kind: str, data: Any) -> dict[str, Any]:
        return {
            "id": id_,
            "contract": contract,
            "event_type": kind,
            "ledger": 10 + id_,
            "timestamp": None,
            "data": data,
        }

    client, http = make_client(
        {
            "/v1/assets/1": asset_payload(),
            "/v1/events": [
                event(1, TOKEN, "transfer", {"from": "A", "to": "B", "amount": HUGE}),
                event(2, TOKEN, "mint", {"amount": "5"}),
                event(3, "COTHER", "transfer", {"amount": "5"}),
                event(4, TOKEN, "transfer", "not-a-dict"),
            ],
        }
    )

    frame = await client.get_transfers_df(1)

    assert list(frame["id"]) == [1]
    assert frame["from"].iloc[0] == "A"
    assert frame["amount"].dtype == pd.ArrowDtype(I128_TYPE)
    assert frame["amount"].iloc[0].as_py() == int(HUGE)
    await http.aclose()
