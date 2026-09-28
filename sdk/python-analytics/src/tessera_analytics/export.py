from __future__ import annotations

from typing import TYPE_CHECKING

import numpy as np
import polars as pl
import pyarrow as pa
import plotly.graph_objects as go

if TYPE_CHECKING:
    import pandas as pd


def plot_holder_distribution(
    balances: pl.DataFrame | pa.Table | list[float],
    address_column: str = "address",
    balance_column: str = "balance",
    asset_id: int = 0,
) -> go.Figure:
    """Create an interactive Plotly histogram of token holder distribution.

    Parameters
    ----------
    balances : pl.DataFrame, pa.Table, or list[float]
        Token holder balances.
    address_column : str, default "address"
        Column name for addresses.
    balance_column : str, default "balance"
        Column name for balances.
    asset_id : int, default 0
        Asset identifier for the chart title.

    Returns
    -------
    plotly.graph_objects.Figure
        Interactive histogram of holder balances.
    """
    if isinstance(balances, pl.DataFrame):
        values = balances.select(balance_column).to_series().to_list()
    elif isinstance(balances, pa.Table):
        values = balances.column(balance_column).to_pylist()
    elif isinstance(balances, list):
        values = balances
    else:
        raise TypeError(f"Unsupported type {type(balances).__name__}")

    fig = go.Figure(data=[go.Histogram(x=values, nbinsx=50, marker_color="steelblue")])
    fig.update_layout(
        title=f"Holder Distribution (Asset ID: {asset_id})",
        xaxis_title="Balance",
        yaxis_title="Number of Holders",
        template="plotly_white",
    )
    return fig


def plot_capitalization_curve(
    total_supply: float,
    holder_balances: pl.DataFrame | pa.Table | list[float],
    address_column: str = "address",
    balance_column: str = "balance",
    asset_id: int = 0,
) -> go.Figure:
    """Create a cumulative capitalization curve (Lorenz curve).

    Parameters
    ----------
    total_supply : float
        Total token supply.
    holder_balances : pl.DataFrame, pa.Table, or list[float]
        Token holder balances.
    address_column : str, default "address"
        Column name for addresses.
    balance_column : str, default "balance"
        Column name for balances.
    asset_id : int, default 0
        Asset identifier for the chart title.

    Returns
    -------
    plotly.graph_objects.Figure
        Lorenz curve showing cumulative share of supply vs cumulative holders.
    """
    if isinstance(holder_balances, pl.DataFrame):
        values = holder_balances.select(balance_column).to_series().to_list()
    elif isinstance(holder_balances, pa.Table):
        values = holder_balances.column(balance_column).to_pylist()
    elif isinstance(holder_balances, list):
        values = holder_balances
    else:
        raise TypeError(f"Unsupported type {type(holder_balances).__name__}")

    sorted_balances = np.array([float(v) for v in values])
    total = sorted_balances.sum()
    if total == 0:
        cumulative_share = np.zeros(len(sorted_balances))
    else:
        cumulative_share = np.cumsum(sorted_balances) / total

    holder_share = np.linspace(0, 1, len(sorted_balances))

    fig = go.Figure()
    fig.add_trace(go.Scatter(
        x=[0] + holder_share.tolist(),
        y=[0] + cumulative_share.tolist(),
        mode="lines",
        name="Lorenz Curve",
        line=dict(color="blue", width=2),
    ))
    fig.add_trace(go.Scatter(
        x=[0, 1],
        y=[0, 1],
        mode="lines",
        name="Equality Line",
        line=dict(color="red", dash="dash", width=1),
    ))
    fig.update_layout(
        title=f"Capitalization Curve (Asset ID: {asset_id})",
        xaxis_title="Cumulative Share of Holders",
        yaxis_title="Cumulative Share of Supply",
        template="plotly_white",
    )
    return fig


def plot_drip_yield_forecast(
    forecast: dict,
    asset_id: int = 0,
) -> go.Figure:
    """Create an interactive forecast chart for drip yield projections.

    Parameters
    ----------
    forecast : dict
        Dictionary containing 'historical_yields', 'projected_yields',
        'confidence_upper', 'confidence_lower'.
    asset_id : int, default 0
        Asset identifier for the chart title.

    Returns
    -------
    plotly.graph_objects.Figure
        Interactive line chart with confidence intervals.
    """
    hist = forecast.get("historical_yields", [])
    proj = forecast.get("projected_yields", [])
    upper = forecast.get("confidence_upper", proj)
    lower = forecast.get("confidence_lower", proj)

    hist_x = list(range(len(hist)))
    proj_x = list(range(len(hist), len(hist) + len(proj)))

    fig = go.Figure()
    fig.add_trace(go.Scatter(
        x=hist_x, y=hist, mode="lines", name="Historical",
        line=dict(color="blue"),
    ))
    fig.add_trace(go.Scatter(
        x=proj_x, y=proj, mode="lines", name="Projected",
        line=dict(color="red", dash="dash"),
    ))
    fig.add_trace(go.Scatter(
        x=proj_x, y=upper, mode="lines", name="Upper Bound",
        line=dict(width=0),
        showlegend=False,
    ))
    fig.add_trace(go.Scatter(
        x=proj_x, y=lower, mode="lines", name="Confidence Interval",
        fill="tonexty", fillcolor="rgba(255,0,0,0.1)",
        line=dict(width=0),
    ))
    fig.update_layout(
        title=f"Drip Yield Forecast (Asset ID: {asset_id})",
        xaxis_title="Period",
        yaxis_title="Yield",
        template="plotly_white",
    )
    return fig


def export_to_notebook(
    analysis: dict,
    asset_id: int = 0,
    asset_name: str = "Unknown Asset",
) -> str:
    """Generate a Jupyter Notebook string with inline matplotlib charts
    from cap-table analysis results.

    Parameters
    ----------
    analysis : dict
        Dictionary containing analysis results with keys like
        'holder_metrics', 'retention_metrics', 'drip_forecast', etc.
    asset_id : int, default 0
        Asset identifier.
    asset_name : str, default "Unknown Asset"
        Asset name for the notebook title.

    Returns
    -------
    str
        Python source code string representing a Jupyter Notebook cell sequence.

    Examples
    --------
    >>> analysis = {
    ...     "holder_metrics": {"total_holders": 100, "gini_coefficient": 0.35},
    ...     "retention_metrics": [{"period": "30d", "retention_rate": 0.85}],
    ... }
    >>> notebook_code = export_to_notebook(analysis, asset_id=1)
    >>> "tessera_analytics" in notebook_code
    True
    """
    lines: list[str] = []

    lines.append(f"# Tessera Analytics: {asset_name} (Asset ID: {asset_id})")
    lines.append("")
    lines.append("```python")
    lines.append("import tessera_analytics")
    lines.append("import polars as pl")
    lines.append("import plotly.graph_objects as go")
    lines.append("from matplotlib import pyplot as plt")
    lines.append("import numpy as np")
    lines.append("```")
    lines.append("")

    lines.append("## Holder Distribution")
    lines.append("")
    lines.append("```python")
    lines.append("# Generate holder distribution chart")
    lines.append(f"fig = plot_holder_distribution(balances, asset_id={asset_id})")
    lines.append("fig.show()")
    lines.append("```")
    lines.append("")

    if "holder_metrics" in analysis:
        metrics = analysis["holder_metrics"]
        gini = metrics.get("gini_coefficient", 0)
        lines.append("## Gini Coefficient Analysis")
        lines.append("")
        lines.append("```python")
        lines.append(f"# Gini coefficient: {gini:.4f}")
        lines.append("gini = calculate_gini_coefficient(balances)")
        lines.append(f"print(f'Gini Coefficient: {{gini:.4f}}')")
        lines.append("```")
        lines.append("")

    lines.append("## Capitalization Curve")
    lines.append("")
    lines.append("```python")
    lines.append("# Generate Lorenz curve")
    lines.append(f"fig = plot_capitalization_curve(total_supply, balances, asset_id={asset_id})")
    lines.append("fig.show()")
    lines.append("```")
    lines.append("")

    if "retention_metrics" in analysis:
        retention = analysis["retention_metrics"]
        lines.append("## Holder Retention Analysis")
        lines.append("")
        lines.append("```python")
        lines.append("# Plot retention metrics")
        lines.append("retention_data = [")
        for r in retention:
            lines.append(f"    {r},")
        lines.append("]")
        lines.append("plt.figure(figsize=(10, 5))")
        lines.append("plt.bar([r['period'] for r in retention_data], [r['retention_rate'] for r in retention_data])")
        lines.append("plt.title('Holder Retention Rate by Period')")
        lines.append("plt.ylabel('Retention Rate')")
        lines.append("plt.show()")
        lines.append("```")
        lines.append("")

    if "drip_forecast" in analysis:
        forecast = analysis["drip_forecast"]
        lines.append("## Drip Yield Forecast")
        lines.append("")
        lines.append("```python")
        lines.append("# Plot yield forecast with confidence intervals")
        lines.append(f"fig = plot_drip_yield_forecast(forecast_data, asset_id={asset_id})")
        lines.append("fig.show()")
        lines.append("```")
        lines.append("")

    lines.append("## Summary")
    lines.append("")
    lines.append("```python")
    lines.append("# Print key metrics")
    lines.append(f"print('Asset: {asset_name}')")
    lines.append(f"print('Asset ID: {asset_id}')")
    if "holder_metrics" in analysis:
        metrics = analysis["holder_metrics"]
        lines.append(f"print(f'Total Holders: {{metrics[\"total_holders\"]}}')")
        lines.append(f"print(f'Gini Coefficient: {{metrics[\"gini_coefficient\"]:.4f}}')")
    lines.append("```")
    lines.append("")

    return "\n".join(lines)
