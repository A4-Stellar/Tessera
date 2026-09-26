from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from types import TracebackType
from typing import Any, TypeVar, overload
from urllib.parse import quote

import httpx
from pandas import DataFrame
from pydantic import TypeAdapter, ValidationError

from .exceptions import (
    APIError,
    ConnectionError,
    NotFoundError,
    RateLimitError,
    ResponseError,
)
from .models import (
    AddressCompliance,
    AddressHolding,
    Asset,
    AssetEvents,
    ComplianceSummary,
    Distribution,
    Event,
    Holder,
    SparseAsset,
    Stats,
)

T = TypeVar("T")
QueryParams = Mapping[str, str | int | bool | None]
FieldSelection = str | Sequence[str]
DEFAULT_BASE_URL = "http://localhost:8080"
DEFAULT_TIMEOUT = 30.0
MAX_HOLDERS_PAGE_SIZE = 100

STATS_ADAPTER: TypeAdapter[Stats] = TypeAdapter(Stats)
EVENTS_ADAPTER: TypeAdapter[list[Event]] = TypeAdapter(list[Event])
ASSETS_ADAPTER: TypeAdapter[list[Asset]] = TypeAdapter(list[Asset])
SPARSE_ASSETS_ADAPTER: TypeAdapter[list[SparseAsset]] = TypeAdapter(list[SparseAsset])
ASSET_ADAPTER: TypeAdapter[Asset] = TypeAdapter(Asset)
SPARSE_ASSET_ADAPTER: TypeAdapter[SparseAsset] = TypeAdapter(SparseAsset)
ASSET_EVENTS_ADAPTER: TypeAdapter[AssetEvents] = TypeAdapter(AssetEvents)
HOLDERS_ADAPTER: TypeAdapter[list[Holder]] = TypeAdapter(list[Holder])
COMPLIANCE_ADAPTER: TypeAdapter[ComplianceSummary] = TypeAdapter(ComplianceSummary)
DISTRIBUTIONS_ADAPTER: TypeAdapter[list[Distribution]] = TypeAdapter(list[Distribution])
DISTRIBUTION_ADAPTER: TypeAdapter[Distribution] = TypeAdapter(Distribution)
ADDRESS_HOLDINGS_ADAPTER: TypeAdapter[list[AddressHolding]] = TypeAdapter(
    list[AddressHolding]
)
ADDRESS_COMPLIANCE_ADAPTER: TypeAdapter[list[AddressCompliance]] = TypeAdapter(
    list[AddressCompliance]
)


def _valid_offset(value: int | None, name: str) -> None:
    if value is not None and (
        isinstance(value, bool) or not isinstance(value, int) or value < 0
    ):
        raise ValueError(f"{name} must be a non-negative integer")


def _valid_id(value: int, name: str) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"{name} must be a non-negative integer")


def _address_segment(address: str) -> str:
    if not isinstance(address, str) or not address:
        raise ValueError("address must be a non-empty string")
    return quote(address, safe="")


def _field_selection(fields: FieldSelection | None) -> str | None:
    if fields is None:
        return None
    values = [fields] if isinstance(fields, str) else list(fields)
    selected: list[str] = []
    for value in values:
        if not isinstance(value, str):
            raise ValueError("fields must contain only strings")
        selected.extend(field.strip() for field in value.split(",") if field.strip())
    if not selected:
        return None
    return ",".join(dict.fromkeys(selected))


def _clean_params(params: QueryParams | None) -> dict[str, str | int | bool]:
    if params is None:
        return {}
    return {key: value for key, value in params.items() if value is not None}


def _retry_after_seconds(response: httpx.Response, body: Any) -> float | None:
    value: str | int | float | None = response.headers.get("retry-after")
    if value is None and isinstance(body, dict):
        candidate = body.get("retry_after")
        if isinstance(candidate, str | int | float):
            value = candidate
    if value is None:
        return None
    try:
        seconds = float(value)
    except ValueError:
        try:
            retry_at = parsedate_to_datetime(str(value))
        except (TypeError, ValueError, OverflowError):
            return None
        if retry_at.tzinfo is None:
            retry_at = retry_at.replace(tzinfo=timezone.utc)
        seconds = (retry_at - datetime.now(timezone.utc)).total_seconds()
    return max(0.0, seconds)


def _error_details(response: httpx.Response) -> tuple[str, str | None, Any]:
    response_text = response.text.strip()
    try:
        body = json.loads(response_text) if response_text else None
    except (json.JSONDecodeError, UnicodeDecodeError):
        body = response_text
    error_code: str | None = None
    message: str | None = None
    if isinstance(body, dict):
        candidate_code = body.get("error")
        if isinstance(candidate_code, str):
            error_code = candidate_code
        for key in ("message", "detail"):
            candidate_message = body.get(key)
            if isinstance(candidate_message, str) and candidate_message.strip():
                message = candidate_message.strip()
                break
    elif isinstance(body, str) and body.strip():
        message = body.strip()
    if message is None and error_code:
        message = error_code
    if message is None:
        message = response_text or response.reason_phrase or "HTTP request failed"
    return message, error_code, body


def _raise_http_error(response: httpx.Response) -> None:
    message, error_code, parsed_body = _error_details(response)
    retry_after = _retry_after_seconds(response, parsed_body)
    if response.status_code == 404:
        error_type: type[APIError] = NotFoundError
    elif response.status_code == 429:
        error_type = RateLimitError
    else:
        error_type = APIError
    raise error_type(
        message,
        status_code=response.status_code,
        error_code=error_code,
        body=parsed_body,
        response_text=response.text,
        retry_after=retry_after,
        request=response.request,
    )


class TesseraClient:
    def __init__(
        self,
        base_url: str | httpx.URL = DEFAULT_BASE_URL,
        *,
        client: httpx.AsyncClient | None = None,
        timeout: float | httpx.Timeout | None = DEFAULT_TIMEOUT,
        headers: Mapping[str, str] | None = None,
    ) -> None:
        self.base_url = str(base_url).rstrip("/")
        if not self.base_url:
            raise ValueError("base_url must not be empty")
        parsed_url = httpx.URL(self.base_url)
        if (
            not parsed_url.is_absolute_url
            or parsed_url.scheme not in {"http", "https"}
            or not parsed_url.host
        ):
            raise ValueError("base_url must be an absolute HTTP(S) URL")
        self._owns_http_client = client is None
        default_headers = {
            "Accept": "application/json",
            "User-Agent": "tessera-sdk/0.1.0",
        }
        if headers:
            default_headers.update(headers)
        self._headers = default_headers
        self._http_client = client or httpx.AsyncClient(
            timeout=timeout,
            headers=self._headers,
        )

    @property
    def http_client(self) -> httpx.AsyncClient:
        return self._http_client

    @property
    def owns_http_client(self) -> bool:
        return self._owns_http_client

    @property
    def is_closed(self) -> bool:
        return self._http_client.is_closed

    async def __aenter__(self) -> TesseraClient:
        if self._http_client.is_closed:
            raise RuntimeError("the HTTP client is already closed")
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        if self._owns_http_client:
            await self._http_client.aclose()

    def _url(self, path: str) -> str:
        return f"{self.base_url}/v1{path}"

    async def _get(
        self,
        path: str,
        adapter: TypeAdapter[T],
        params: QueryParams | None = None,
    ) -> T:
        try:
            response = await self._http_client.get(
                self._url(path),
                params=_clean_params(params),
                headers=self._headers,
            )
        except httpx.RequestError as exc:
            raise ConnectionError(str(exc), request=exc.request) from exc
        if not response.is_success:
            _raise_http_error(response)
        try:
            payload = json.loads(response.text)
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            raise ResponseError(
                f"invalid JSON response: {exc}",
                status_code=response.status_code,
                response_text=response.text,
                request=response.request,
            ) from exc
        try:
            return adapter.validate_python(payload)
        except ValidationError as exc:
            raise ResponseError(
                f"response validation failed: {exc}",
                status_code=response.status_code,
                response_text=response.text,
                request=response.request,
            ) from exc

    async def get_stats(self) -> Stats:
        return await self._get("/stats", STATS_ADAPTER)

    async def get_events(self) -> list[Event]:
        return await self._get("/events", EVENTS_ADAPTER)

    @overload
    async def get_assets(
        self,
        *,
        asset_type: str | None = None,
        active: bool | None = None,
        offset: int | None = None,
        limit: int | None = None,
        fields: None = None,
    ) -> list[Asset]: ...

    @overload
    async def get_assets(
        self,
        *,
        asset_type: str | None = None,
        active: bool | None = None,
        offset: int | None = None,
        limit: int | None = None,
        fields: FieldSelection,
    ) -> list[SparseAsset]: ...

    async def get_assets(
        self,
        *,
        asset_type: str | None = None,
        active: bool | None = None,
        offset: int | None = None,
        limit: int | None = None,
        fields: FieldSelection | None = None,
    ) -> list[Asset] | list[SparseAsset]:
        _valid_offset(offset, "offset")
        _valid_offset(limit, "limit")
        selected = _field_selection(fields)
        params: QueryParams = {
            "asset_type": asset_type,
            "active": active,
            "offset": offset,
            "limit": limit,
            "fields": selected,
        }
        if selected is None:
            return await self._get("/assets", ASSETS_ADAPTER, params)
        return await self._get("/assets", SPARSE_ASSETS_ADAPTER, params)

    @overload
    async def get_asset(
        self,
        asset_id: int,
        *,
        fields: None = None,
    ) -> Asset: ...

    @overload
    async def get_asset(
        self,
        asset_id: int,
        *,
        fields: FieldSelection,
    ) -> SparseAsset: ...

    async def get_asset(
        self,
        asset_id: int,
        *,
        fields: FieldSelection | None = None,
    ) -> Asset | SparseAsset:
        _valid_id(asset_id, "asset_id")
        selected = _field_selection(fields)
        if selected is None:
            return await self._get(
                f"/assets/{asset_id}",
                ASSET_ADAPTER,
                {"fields": None},
            )
        return await self._get(
            f"/assets/{asset_id}",
            SPARSE_ASSET_ADAPTER,
            {"fields": selected},
        )

    async def get_asset_events(
        self,
        asset_id: int,
        *,
        include_diagnostics: bool = False,
    ) -> AssetEvents:
        _valid_id(asset_id, "asset_id")
        return await self._get(
            f"/assets/{asset_id}/events",
            ASSET_EVENTS_ADAPTER,
            {"include_diagnostics": include_diagnostics},
        )

    async def get_holders(
        self,
        asset_id: int,
        *,
        offset: int | None = None,
        limit: int | None = None,
    ) -> list[Holder]:
        _valid_id(asset_id, "asset_id")
        _valid_offset(offset, "offset")
        _valid_offset(limit, "limit")
        return await self._get(
            f"/assets/{asset_id}/holders",
            HOLDERS_ADAPTER,
            {"offset": offset, "limit": limit},
        )

    async def get_holders_dataframe(self, asset_id: int) -> DataFrame:
        _valid_id(asset_id, "asset_id")
        holders: list[Holder] = []
        offset = 0
        while True:
            page = await self.get_holders(
                asset_id,
                offset=offset,
                limit=MAX_HOLDERS_PAGE_SIZE,
            )
            holders.extend(page)
            if len(page) < MAX_HOLDERS_PAGE_SIZE:
                break
            offset += len(page)
        frame = DataFrame.from_records(
            [holder.model_dump(mode="python") for holder in holders],
            columns=["address", "balance", "share_percent"],
        )
        frame["balance"] = frame["balance"].astype("string")
        return frame

    async def get_asset_compliance(self, asset_id: int) -> ComplianceSummary:
        _valid_id(asset_id, "asset_id")
        return await self._get(
            f"/assets/{asset_id}/compliance",
            COMPLIANCE_ADAPTER,
        )

    async def get_dividends(self, asset_id: int) -> list[Distribution]:
        _valid_id(asset_id, "asset_id")
        return await self._get(
            f"/assets/{asset_id}/dividends",
            DISTRIBUTIONS_ADAPTER,
        )

    async def get_distribution(
        self,
        asset_id: int,
        distribution_id: int,
    ) -> Distribution:
        _valid_id(asset_id, "asset_id")
        _valid_id(distribution_id, "distribution_id")
        return await self._get(
            f"/assets/{asset_id}/distributions/{distribution_id}",
            DISTRIBUTION_ADAPTER,
        )

    async def get_address_holdings(self, address: str) -> list[AddressHolding]:
        segment = _address_segment(address)
        return await self._get(
            f"/holders/{segment}",
            ADDRESS_HOLDINGS_ADAPTER,
        )

    async def get_holders_compliance(
        self,
        address: str,
    ) -> list[AddressCompliance]:
        segment = _address_segment(address)
        return await self._get(
            f"/holders/{segment}/compliance",
            ADDRESS_COMPLIANCE_ADAPTER,
        )

    async def get_address_compliance(
        self,
        address: str,
    ) -> list[AddressCompliance]:
        segment = _address_segment(address)
        return await self._get(
            f"/compliance/{segment}",
            ADDRESS_COMPLIANCE_ADAPTER,
        )


AsyncTesseraClient = TesseraClient


__all__ = ["AsyncTesseraClient", "TesseraClient"]
