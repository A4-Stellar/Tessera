from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import polars as pl
import pyarrow as pa


@dataclass
class HolderMetrics:
    total_holders: int
    total_balance: float
    mean_balance: float
    median_balance: float
    min_balance: float
    max_balance: float
    std_balance: float
    gini_coefficient: float
    top_10_percent_share: float
    top_1_percent_share: float


@dataclass
class RetentionMetrics:
    period: str
    total_holders_start: int
    total_holders_end: int
    retained_holders: int
    new_holders: int
    departed_holders: int
    retention_rate: float
    churn_rate: float
    average_balance_change: float


@dataclass
class DripYieldForecast:
    asset_id: int
    forecast_periods: int
    historical_yields: list[float]
    projected_yields: list[float]
    confidence_upper: list[float]
    confidence_lower: list[float]
    mean_projected_yield: float
    total_projected_yield: float
    annualized_yield_rate: float
    model_metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class CapTableAnalysis:
    asset_id: int
    asset_name: str
    total_supply: str
    holder_count: int
    gini_coefficient: float
    holder_metrics: HolderMetrics
    retention_metrics: list[RetentionMetrics]
    concentration_index: float
    herfindahl_index: float
    top_holders: list[dict[str, Any]]
    distribution_pandas_frame: pl.DataFrame
    distribution_arrow_table: pa.Table
