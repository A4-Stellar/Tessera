from __future__ import annotations

from collections.abc import Iterable
from decimal import Decimal

import pandas as pd
import pyarrow as pa
from tessera_sdk import TesseraClient

# 39 digits hold every signed 128-bit integer (max 1.7e38), which
# ``decimal128(38, 0)`` cannot.
I128_TYPE = pa.decimal256(39, 0)


def to_i128_series(values: Iterable[str]) -> pd.Series:
    """Convert decimal-string balances to an exact Arrow-backed Series."""
    array = pa.array([Decimal(value) for value in values], type=I128_TYPE)
    return pd.Series(pd.arrays.ArrowExtensionArray(array))


class TesseraPandasClient(TesseraClient):
    """``TesseraClient`` with DataFrame helpers for data scientists."""

    async def get_holders_df(self, asset_id: int) -> pd.DataFrame:
        """Cap table: ``address``, ``balance`` (Arrow int128), ``share_percent``."""
        frame = await self.get_holders_dataframe(asset_id)
        frame["balance"] = to_i128_series(frame["balance"])
        return frame

    async def get_transfers_df(self, asset_id: int) -> pd.DataFrame:
        """Transfer history of the asset's token from ``GET /v1/events``.

        Columns: ``id``, ``ledger``, ``timestamp``, ``from``, ``to`` and
        ``amount`` (Arrow int128). ``from``/``to`` are ``None`` when the
        indexed event data does not carry them.
        """
        asset = await self.get_asset(asset_id)
        rows = []
        for event in await self.get_events():
            data = event.data if isinstance(event.data, dict) else {}
            if (
                event.event_type.lower() == "transfer"
                and event.contract == asset.token_contract
                and "amount" in data
            ):
                rows.append(
                    {
                        "id": event.id,
                        "ledger": event.ledger,
                        "timestamp": event.timestamp,
                        "from": data.get("from"),
                        "to": data.get("to"),
                        "amount": str(data["amount"]),
                    }
                )
        frame = pd.DataFrame.from_records(
            rows, columns=["id", "ledger", "timestamp", "from", "to", "amount"]
        )
        frame["amount"] = to_i128_series(frame["amount"])
        return frame
