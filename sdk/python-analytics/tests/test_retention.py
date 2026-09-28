from __future__ import annotations

import polars as pl
import pytest

from tessera_analytics import (
    compute_holder_retention,
    forecast_drip_yield,
)


class TestComputeHolderRetention:
    def test_full_retention(self):
        """All start holders present at end -> retention = 1."""
        start = pl.DataFrame({
            "address": ["A", "B", "C"],
            "balance": [100.0, 200.0, 300.0],
        })
        end = pl.DataFrame({
            "address": ["A", "B", "C"],
            "balance": [110.0, 220.0, 330.0],
        })
        metrics = compute_holder_retention(start, end, period="30d")
        assert metrics.retention_rate == pytest.approx(1.0)
        assert metrics.churn_rate == pytest.approx(0.0)
        assert metrics.retained_holders == 3
        assert metrics.new_holders == 0
        assert metrics.departed_holders == 0

    def test_no_retention(self):
        """No start holders present at end -> retention = 0."""
        start = pl.DataFrame({
            "address": ["A", "B", "C"],
            "balance": [100.0, 200.0, 300.0],
        })
        end = pl.DataFrame({
            "address": ["D", "E", "F"],
            "balance": [100.0, 200.0, 300.0],
        })
        metrics = compute_holder_retention(start, end, period="30d")
        assert metrics.retention_rate == pytest.approx(0.0)
        assert metrics.churn_rate == pytest.approx(1.0)

    def test_partial_retention(self):
        """Some holders retained, some new, some departed."""
        start = pl.DataFrame({
            "address": ["A", "B", "C"],
            "balance": [100.0, 200.0, 300.0],
        })
        end = pl.DataFrame({
            "address": ["A", "B", "D"],
            "balance": [110.0, 220.0, 150.0],
        })
        metrics = compute_holder_retention(start, end, period="30d")
        assert metrics.retention_rate == pytest.approx(2 / 3)
        assert metrics.retained_holders == 2
        assert metrics.new_holders == 1
        assert metrics.departed_holders == 1

    def test_from_pyarrow(self):
        """Test with pyarrow Tables."""
        import pyarrow as pa

        start = pa.table({
            "address": ["A", "B"],
            "balance": [100.0, 200.0],
        })
        end = pa.table({
            "address": ["A"],
            "balance": [110.0],
        })
        metrics = compute_holder_retention(start, end, period="7d")
        assert metrics.retention_rate == pytest.approx(0.5)
        assert metrics.period == "7d"

    def test_empty_raises(self):
        """Empty inputs should raise ValueError."""
        start = pl.DataFrame({"address": [], "balance": []})
        end = pl.DataFrame({"address": ["A"], "balance": [100.0]})
        with pytest.raises(ValueError):
            compute_holder_retention(start, end)

    def test_balance_changes(self):
        """Verify average balance change calculation."""
        start = pl.DataFrame({
            "address": ["A", "B"],
            "balance": [100.0, 200.0],
        })
        end = pl.DataFrame({
            "address": ["A", "B"],
            "balance": [150.0, 180.0],
        })
        metrics = compute_holder_retention(start, end, period="30d")
        # A: +50, B: -20, avg = 15
        assert metrics.average_balance_change == pytest.approx(15.0, abs=1e-6)

    def test_retention_metrics_fields(self):
        """Verify all RetentionMetrics fields are populated."""
        start = pl.DataFrame({
            "address": ["A", "B", "C"],
            "balance": [100.0, 200.0, 300.0],
        })
        end = pl.DataFrame({
            "address": ["A", "B", "C"],
            "balance": [110.0, 220.0, 330.0],
        })
        metrics = compute_holder_retention(start, end, period="30d")
        assert hasattr(metrics, "period")
        assert hasattr(metrics, "total_holders_start")
        assert hasattr(metrics, "total_holders_end")
        assert hasattr(metrics, "retained_holders")
        assert hasattr(metrics, "new_holders")
        assert hasattr(metrics, "departed_holders")
        assert hasattr(metrics, "retention_rate")
        assert hasattr(metrics, "churn_rate")
        assert hasattr(metrics, "average_balance_change")
