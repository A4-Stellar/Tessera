# Tessera Analytics (tessera-analytics)

`tessera-analytics` is a specialized Python data analytics library optimized for quantitative financial modeling, cap-table analysis, and yield forecasting on the Tessera RWA platform.

## Install

```bash
python -m pip install tessera-analytics
```

## Features

- **Native Polars and PyArrow Integration** — ultra-fast dataframe generation for cap-table analysis
- **Quantitative Financial Functions** — Gini coefficient calculation, holder retention analysis, and drip yield forecasting
- **Jupyter Notebook Export** — inline matplotlib and plotly charts for interactive analysis

## Quick Start

### Cap-Table Analysis with Polars

```python
import polars as pl
from tessera_analytics import calculate_gini_coefficient

# Token holder balances
balances = pl.DataFrame({
    "address": ["A", "B", "C", "D"],
    "balance": [1000.0, 2500.0, 500.0, 6000.0],
})

gini = calculate_gini_coefficient(balances)
print(f"Gini Coefficient: {gini:.4f}")
```

### Holder Retention Analysis

```python
import polars as pl
from tessera_analytics import compute_holder_retention

start = pl.DataFrame({
    "address": ["A", "B", "C"],
    "balance": [100.0, 200.0, 300.0],
})
end = pl.DataFrame({
    "address": ["A", "B", "D"],
    "balance": [110.0, 220.0, 150.0],
})

metrics = compute_holder_retention(start, end, period="30d")
print(f"Retention Rate: {metrics.retention_rate:.2%}")
print(f"Churn Rate: {metrics.churn_rate:.2%}")
```

### Drip Yield Forecasting

```python
from tessera_analytics import forecast_drip_yield

historical_yields = [1.0, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.4]
forecast = forecast_drip_yield(historical_yields, forecast_periods=12)

print(f"Mean Projected Yield: {forecast.mean_projected_yield:.4f}")
print(f"Total Projected Yield: {forecast.total_projected_yield:.4f}")
```

### Jupyter Notebook Export

```python
from tessera_analytics import export_to_notebook

analysis = {
    "holder_metrics": {
        "total_holders": 100,
        "gini_coefficient": 0.35,
    },
    "retention_metrics": [
        {"period": "30d", "retention_rate": 0.85},
        {"period": "60d", "retention_rate": 0.78},
    ],
}

notebook_code = export_to_notebook(analysis, asset_id=1, asset_name="My Token")
print(notebook_code)
```

## Visualization

All plotting functions return interactive Plotly figures or matplotlib charts compatible with Jupyter Notebooks.

```python
from tessera_analytics import (
    plot_holder_distribution,
    plot_capitalization_curve,
    plot_drip_yield_forecast,
)

# Interactive holder distribution histogram
fig = plot_holder_distribution(balances)
fig.show()

# Lorenz curve for capital concentration
fig = plot_capitalization_curve(total_supply=10000, balances=balances)
fig.show()

# Yield forecast with confidence intervals
fig = plot_drip_yield_forecast(forecast_data)
fig.show()
```

## API Reference

### `calculate_gini_coefficient(balances, address_column="address", balance_column="balance")`

Calculates the Gini coefficient for token holder distribution. Accepts `polars.DataFrame`, `pyarrow.Table`, or `list[float]`.

### `compute_holder_retention(holders_start, holders_end, address_column="address", balance_column="balance", period="30d")`

Computes holder retention metrics between two time periods. Returns a `RetentionMetrics` object.

### `forecast_drip_yield(historical_yields, forecast_periods=12, confidence_level=0.95, asset_id=0)`

Forecasts future drip yields using OLS linear regression. Returns a `DripYieldForecast` object with projections and confidence intervals.

### `export_to_notebook(analysis, asset_id=0, asset_name="Unknown Asset")`

Generates a Jupyter Notebook string with inline matplotlib and plotly charts from analysis results.

## Dependencies

- `polars>=0.20,<1` — fast dataframe operations
- `pyarrow>=14,<18` — columnar memory format
- `pandas>=2.0,<3` — data manipulation
- `matplotlib>=3.7,<4` — static charts
- `plotly>=5.18,<6` — interactive charts
- `numpy>=1.24,<3` — numerical computing
- `scipy>=1.11,<2` — statistical functions

## Development

```bash
cd sdk/python-analytics
python -m venv .venv
. .venv/bin/activate
python -m pip install -e ".[test]"
ruff format --check .
ruff check .
mypy
pytest
```

## License

Apache-2.0
