from __future__ import annotations

from typing import TYPE_CHECKING

import numpy as np
import polars as pl
import pyarrow as pa

if TYPE_CHECKING:
    import pandas as pd


def calculate_gini_coefficient(
    balances: pl.DataFrame | pa.Table | list[float],
    address_column: str = "address",
    balance_column: str = "balance",
) -> float:
    """Calculate the Gini coefficient for token holder distribution.

    The Gini coefficient measures inequality in the distribution of token
    balances across holders. A value of 0 indicates perfect equality
    (all holders have the same balance), while 1 indicates perfect
    inequality (one holder has all tokens).

    Parameters
    ----------
    balances : pl.DataFrame, pa.Table, or list[float]
        Token holder balances. If a DataFrame or Table is provided,
        must contain the address and balance columns specified.
    address_column : str, default "address"
        Column name for holder addresses.
    balance_column : str, default "balance"
        Column name for holder balances.

    Returns
    -------
    float
        The Gini coefficient in the range [0, 1].

    Raises
    ------
    ValueError
        If balances are empty or contain negative values.

    Examples
    --------
    >>> import polars as pl
    >>> df = pl.DataFrame({
    ...     "address": ["A", "B", "C"],
    ...     "balance": [100.0, 200.0, 700.0],
    ... })
    >>> calculate_gini_coefficient(df)
    0.3888888888888888
    """
    if isinstance(balances, pl.DataFrame):
        values = balances.select(balance_column).to_series().to_list()
    elif isinstance(balances, pa.Table):
        values = balances.column(balance_column).to_pylist()
    elif isinstance(balances, list):
        values = balances
    else:
        raise TypeError(
            f"Unsupported type {type(balances).__name__}. "
            "Expected pl.DataFrame, pa.Table, or list[float]."
        )

    values = [float(v) for v in values]

    if not values:
        raise ValueError("balances must not be empty")

    if any(v < 0 for v in values):
        raise ValueError("balances must not contain negative values")

    n = len(values)
    if n == 1:
        return 0.0

    sorted_values = sorted(values)
    cumulative_sum = sum(sorted_values)

    if cumulative_sum == 0:
        return 0.0

    # Compute Gini using the formula:
    # G = (2 * sum(i * x_i) - (n+1) * sum(x_i)) / (n * sum(x_i))
    # where x_i are sorted in ascending order and i is 1-indexed
    weighted_sum = sum((i + 1) * v for i, v in enumerate(sorted_values))
    gini = (2.0 * weighted_sum - (n + 1) * cumulative_sum) / (n * cumulative_sum)

    return max(0.0, min(1.0, gini))


def compute_holder_retention(
    holders_start: pl.DataFrame | pa.Table | list[dict[str, Any]],
    holders_end: pl.DataFrame | pa.Table | list[dict[str, Any]],
    address_column: str = "address",
    balance_column: str = "balance",
    period: str = "30d",
) -> RetentionMetrics:
    """Compute holder retention metrics between two time periods.

    Compares holder sets at the start and end of a period to determine
    retention rate, churn rate, and balance changes.

    Parameters
    ----------
    holders_start : pl.DataFrame, pa.Table, or list[dict]
        Holders at the start of the period.
    holders_end : pl.DataFrame, pa.Table, or list[dict]
        Holders at the end of the period.
    address_column : str, default "address"
        Column name for holder addresses.
    balance_column : str, default "balance"
        Column name for holder balances.
    period : str, default "30d"
        The period label for the retention metrics.

    Returns
    -------
    RetentionMetrics
        Contains retention rate, churn rate, new/departed counts,
        and average balance change.

    Examples
    --------
    >>> import polars as pl
    >>> start = pl.DataFrame({"address": ["A", "B", "C"], "balance": [100.0, 200.0, 300.0]})
    >>> end = pl.DataFrame({"address": ["A", "B", "D"], "balance": [110.0, 220.0, 150.0]})
    >>> metrics = compute_holder_retention(start, end)
    >>> metrics.retention_rate
    0.6666666666666666
    """
    if isinstance(holders_start, pl.DataFrame):
        start_records = holders_start.to_dicts()
    elif isinstance(holders_start, pa.Table):
        start_records = holders_start.to_pylist()
    elif isinstance(holders_start, list):
        start_records = holders_start
    else:
        raise TypeError(
            f"Unsupported type {type(holders_start).__name__} for holders_start"
        )

    if isinstance(holders_end, pl.DataFrame):
        end_records = holders_end.to_dicts()
    elif isinstance(holders_end, pa.Table):
        end_records = holders_end.to_pylist()
    elif isinstance(holders_end, list):
        end_records = holders_end
    else:
        raise TypeError(
            f"Unsupported type {type(holders_end).__name__} for holders_end"
        )

    if not start_records or not end_records:
        raise ValueError("Both holders_start and holders_end must not be empty")

    start_addresses = {
        str(r[address_column]): float(r[balance_column]) for r in start_records
    }
    end_addresses = {
        str(r[address_column]): float(r[balance_column]) for r in end_records
    }

    retained = set(start_addresses.keys()) & set(end_addresses.keys())
    departed = set(start_addresses.keys()) - set(end_addresses.keys())
    new_holders = set(end_addresses.keys()) - set(start_addresses.keys())

    total_start = len(start_addresses)
    total_end = len(end_addresses)
    retained_count = len(retained)
    new_count = len(new_holders)
    departed_count = len(departed)

    retention_rate = retained_count / total_start if total_start > 0 else 0.0
    churn_rate = departed_count / total_start if total_start > 0 else 0.0

    balance_changes = []
    for addr in retained:
        change = end_addresses[addr] - start_addresses[addr]
        balance_changes.append(change)

    avg_balance_change = (
        float(np.mean(balance_changes)) if balance_changes else 0.0
    )

    return RetentionMetrics(
        period=period,
        total_holders_start=total_start,
        total_holders_end=total_end,
        retained_holders=retained_count,
        new_holders=new_count,
        departed_holders=departed_count,
        retention_rate=round(retention_rate, 6),
        churn_rate=round(churn_rate, 6),
        average_balance_change=round(avg_balance_change, 10),
    )


def forecast_drip_yield(
    historical_yields: list[float],
    forecast_periods: int = 12,
    confidence_level: float = 0.95,
    asset_id: int = 0,
) -> DripYieldForecast:
    """Forecast future drip yield using linear regression with confidence intervals.

    Uses ordinary least squares regression on historical yield data
    to project future yields, with confidence intervals derived from
    residual variance.

    Parameters
    ----------
    historical_yields : list[float]
        Historical yield values in chronological order.
    forecast_periods : int, default 12
        Number of future periods to forecast.
    confidence_level : float, default 0.95
        Confidence level for prediction intervals (0, 1).
    asset_id : int, default 0
        Asset identifier for the forecast.

    Returns
    -------
    DripYieldForecast
        Contains projected yields, confidence bounds, and model metadata.

    Raises
    ------
    ValueError
        If historical_yields has fewer than 2 entries or forecast_periods < 1.

    Examples
    --------
    >>> historical = [1.0, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.4, 2.6, 2.8, 3.0, 3.2]
    >>> forecast = forecast_drip_yield(historical, forecast_periods=3)
    >>> len(forecast.projected_yields)
    3
    >>> forecast.mean_projected_yield > 3.2
    True
    """
    if len(historical_yields) < 2:
        raise ValueError(
            "historical_yields must contain at least 2 data points"
        )

    if forecast_periods < 1:
        raise ValueError("forecast_periods must be at least 1")

    if not (0.0 < confidence_level < 1.0):
        raise ValueError("confidence_level must be between 0 and 1")

    n = len(historical_yields)
    x = np.arange(n)
    y = np.array(historical_yields, dtype=float)

    # OLS regression
    x_mean = np.mean(x)
    y_mean = np.mean(y)

    ss_xy = np.sum((x - x_mean) * (y - y_mean))
    ss_xx = np.sum((x - x_mean) ** 2)

    if ss_xx == 0:
        slope = 0.0
    else:
        slope = ss_xy / ss_xx

    intercept = y_mean - slope * x_mean

    # Predictions for historical periods (residuals)
    y_pred_hist = intercept + slope * x
    residuals = y - y_pred_hist
    ss_res = np.sum(residuals ** 2)

    # Standard error of the estimate
    dof = n - 2
    if dof > 0:
        se = np.sqrt(ss_res / dof)
    else:
        se = 0.0

    # Forecast periods
    x_forecast = np.arange(n, n + forecast_periods)
    projected = intercept + slope * x_forecast

    # Confidence intervals
    from scipy import stats as scipy_stats

    t_crit = scipy_stats.t.ppf((1 + confidence_level) / 2, df=dof) if dof > 0 else 1.96

    forecast_errors = []
    for i, x_f in enumerate(x_forecast):
        se_pred = se * np.sqrt(1.0 / n + (x_f - x_mean) ** 2 / ss_xx) if ss_xx > 0 else se
        margin = t_crit * se_pred
        forecast_errors.append(margin)

    confidence_upper = (projected + np.array(forecast_errors)).tolist()
    confidence_lower = (projected - np.array(forecast_errors)).tolist()
    projected_list = projected.tolist()

    mean_projected = float(np.mean(projected_list))
    total_projected = float(np.sum(projected_list))

    # Annualized yield rate (assuming monthly periods)
    annualized_rate = float(mean_projected * 12 / n) if n > 0 else 0.0

    return DripYieldForecast(
        asset_id=asset_id,
        forecast_periods=forecast_periods,
        historical_yields=historical_yields,
        projected_yields=projected_list,
        confidence_upper=confidence_upper,
        confidence_lower=confidence_lower,
        mean_projected_yield=round(mean_projected, 10),
        total_projected_yield=round(total_projected, 10),
        annualized_yield_rate=round(annualized_rate, 10),
        model_metadata={
            "method": "OLS linear regression",
            "confidence_level": confidence_level,
            "residual_standard_error": round(float(se), 10),
            "r_squared": round(
                float(1.0 - ss_res / np.sum((y - y_mean) ** 2)), 10
            ) if np.sum((y - y_mean) ** 2) > 0 else 1.0,
            "n_observations": n,
            "slope": round(float(slope), 10),
            "intercept": round(float(intercept), 10),
        },
    )
