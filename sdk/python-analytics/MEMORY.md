# Tessera Analytics - Data Science Extension

## Overview

This package provides a specialized Python data analytics library for the Tessera RWA platform, optimized for quantitative financial modeling, cap-table analysis, and yield forecasting.

## Architecture

- `tessera_analytics.analytics` — Core statistical functions (Gini coefficient, holder retention, drip yield forecasting)
- `tessera_analytics.export` — Visualization and Jupyter Notebook export utilities
- `tessera_analytics.models` — Data models for analysis results (HolderMetrics, RetentionMetrics, DripYieldForecast, CapTableAnalysis)

## Integration with Tessera SDK

The analytics package integrates with the `tessera-sdk` Python package to consume API data and perform quantitative analysis on token holder distributions, compliance metrics, and dividend distributions.

## Usage with Tessera Client

```python
import asyncio
import polars as pl
from tessera_sdk import TesseraClient
from tessera_analytics import calculate_gini_coefficient

async def main():
    async with TesseraClient("http://localhost:8080") as client:
        holders_df = await client.get_holders_dataframe(1)
        gini = calculate_gini_coefficient(holders_df, balance_column="balance")
        print(f"Gini Coefficient: {gini:.4f}")

asyncio.run(main())
```

## Key Financial Models

### Gini Coefficient
Measures inequality in token distribution. Value 0 = perfect equality, 1 = perfect inequality.

### Holder Retention
Tracks how many holders remain over time, measures churn and new holder acquisition.

### Drip Yield Forecast
Uses OLS regression on historical dividend distribution data to forecast future yields with confidence intervals.

## Data Formats

All functions accept `polars.DataFrame`, `pyarrow.Table`, or native Python lists, providing flexibility for different data pipelines.
