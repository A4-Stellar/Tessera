from typing import Annotated, Any

from pydantic import BaseModel, ConfigDict, Field

NonNegativeInt = Annotated[int, Field(ge=0)]
Uint32 = Annotated[int, Field(ge=0, le=4_294_967_295)]
Uint64 = Annotated[int, Field(ge=0, le=18_446_744_073_709_551_615)]


class TesseraModel(BaseModel):
    model_config = ConfigDict(
        allow_inf_nan=False,
        extra="forbid",
        strict=True,
    )


class Event(TesseraModel):
    id: Uint64
    contract: str
    event_type: str
    ledger: Uint32
    timestamp: str | None
    data: Any


class DiagnosticEvent(TesseraModel):
    contract: str | None
    event_type: str
    topics: list[Any]
    data: Any
    in_successful_contract_call: bool
    error_code: Uint32 | None


class AssetEvents(TesseraModel):
    asset_id: Uint64
    diagnostics: list[DiagnosticEvent]


class Asset(TesseraModel):
    id: Uint64
    token_contract: str
    issuer: str
    name: str
    symbol: str
    asset_type: str
    description: str
    valuation_cents: str
    valuation_usd: float
    decimals: Uint32
    total_supply: str
    holders: NonNegativeInt
    active: bool
    paused: bool
    compliance_contract: str
    created_at_ledger: Uint32
    indexed_at_ledger: Uint32
    index_error: str | None


class SparseAsset(TesseraModel):
    id: Uint64 | None = None
    token_contract: str | None = None
    issuer: str | None = None
    name: str | None = None
    symbol: str | None = None
    asset_type: str | None = None
    description: str | None = None
    valuation_cents: str | None = None
    valuation_usd: float | None = None
    decimals: Uint32 | None = None
    total_supply: str | None = None
    holders: NonNegativeInt | None = None
    active: bool | None = None
    paused: bool | None = None
    compliance_contract: str | None = None
    created_at_ledger: Uint32 | None = None
    indexed_at_ledger: Uint32 | None = None
    index_error: str | None = None


class Holder(TesseraModel):
    address: str
    balance: str
    share_percent: float


class AddressHolding(TesseraModel):
    address: str
    asset_id: Uint64
    asset_name: str
    symbol: str
    balance: str
    share_percent: float


class AddressCompliance(TesseraModel):
    address: str
    asset_id: Uint64
    asset_name: str
    symbol: str
    balance: str
    status: str
    allowed: bool


class JurisdictionCount(TesseraModel):
    jurisdiction: str
    count: NonNegativeInt


class ComplianceSummary(TesseraModel):
    total_records: NonNegativeInt
    approved: NonNegativeInt
    suspended: NonNegativeInt
    rejected: NonNegativeInt
    pending: NonNegativeInt
    with_expiry: NonNegativeInt
    jurisdictions: list[JurisdictionCount]


class Distribution(TesseraModel):
    id: Uint64
    asset_token: str
    payment_token: str
    total_amount: str
    distributed: str
    claimed_percent: float
    overflow_detected: bool
    completed: bool
    created_at_ledger: Uint32
    fiat_equivalent_usd: float | None


class Stats(TesseraModel):
    total_assets: NonNegativeInt
    active_assets: NonNegativeInt
    tvl_cents: str
    tvl_usd: float
    total_holders: NonNegativeInt
    total_distributions: NonNegativeInt
    last_indexed_ledger: Uint32
    last_updated: str | None


class ApiErrorBody(TesseraModel):
    error: str
    message: str


__all__ = [
    "AddressCompliance",
    "AddressHolding",
    "ApiErrorBody",
    "Asset",
    "AssetEvents",
    "ComplianceSummary",
    "DiagnosticEvent",
    "Distribution",
    "Event",
    "Holder",
    "JurisdictionCount",
    "SparseAsset",
    "Stats",
    "TesseraModel",
]
