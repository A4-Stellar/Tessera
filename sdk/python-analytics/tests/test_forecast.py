from __future__ import annotations

import pytest

from tessera_analytics import forecast_drip_yield


class TestForecastDripYield:
    def test_basic_forecast(self):
        """Basic forecast with increasing yields."""
        historical = [1.0, 1.2, 1.4, 1.6, 1.8, 2.0]
        forecast = forecast_drip_yield(historical, forecast_periods=3)
        assert len(forecast.projected_yields) == 3
        assert forecast.mean_projected_yield > 2.0
        assert forecast.forecast_periods == 3
        assert forecast.asset_id == 0

    def test_default_periods(self):
        """Default forecast_periods is 12."""
        historical = [1.0, 2.0, 3.0, 4.0]
        forecast = forecast_drip_yield(historical)
        assert forecast.forecast_periods == 12

    def test_confidence_intervals(self):
        """Confidence intervals should be properly ordered."""
        historical = [1.0, 1.5, 2.0, 2.5, 3.0]
        forecast = forecast_drip_yield(historical, confidence_level=0.95)
        assert len(forecast.confidence_upper) == len(forecast.confidence_lower)
        for i in range(len(forecast.projected_yields)):
            assert forecast.confidence_upper[i] >= forecast.projected_yields[i]
            assert forecast.confidence_lower[i] <= forecast.projected_yields[i]

    def test_custom_asset_id(self):
        """Custom asset_id should be reflected in the result."""
        historical = [1.0, 2.0, 3.0]
        forecast = forecast_drip_yield(historical, asset_id=42)
        assert forecast.asset_id == 42

    def test_custom_confidence_level(self):
        """Custom confidence_level should affect interval width."""
        historical = [1.0, 2.0, 3.0, 4.0, 5.0]
        forecast_90 = forecast_drip_yield(
            historical, confidence_level=0.90
        )
        forecast_99 = forecast_drip_yield(
            historical, confidence_level=0.99
        )
        # 99% intervals should be wider than 90% intervals
        avg_width_90 = sum(
            u - l
            for u, l in zip(
                forecast_90.confidence_upper, forecast_90.confidence_lower
            )
        )
        avg_width_99 = sum(
            u - l
            for u, l in zip(
                forecast_99.confidence_upper, forecast_99.confidence_lower
            )
        )
        assert avg_width_99 > avg_width_90

    def test_insufficient_data_raises(self):
        """Fewer than 2 historical data points should raise ValueError."""
        with pytest.raises(ValueError):
            forecast_drip_yield([1.0])

    def test_invalid_periods_raises(self):
        """forecast_periods < 1 should raise ValueError."""
        with pytest.raises(ValueError):
            forecast_drip_yield([1.0, 2.0], forecast_periods=0)

    def test_invalid_confidence_raises(self):
        """confidence_level outside (0,1) should raise ValueError."""
        with pytest.raises(ValueError):
            forecast_drip_yield([1.0, 2.0], confidence_level=1.5)

    def test_model_metadata(self):
        """Verify model_metadata contains expected keys."""
        historical = [1.0, 2.0, 3.0, 4.0]
        forecast = forecast_drip_yield(historical)
        meta = forecast.model_metadata
        assert "method" in meta
        assert meta["method"] == "OLS linear regression"
        assert "r_squared" in meta
        assert "slope" in meta
        assert "intercept" in meta
        assert "n_observations" in meta
        assert meta["n_observations"] == 4

    def test_total_projected_yield(self):
        """Total projected yield should equal sum of projected yields."""
        historical = [1.0, 2.0, 3.0]
        forecast = forecast_drip_yield(historical, forecast_periods=2)
        assert forecast.total_projected_yield == pytest.approx(
            sum(forecast.projected_yields)
        )

    def test_annualized_yield_rate(self):
        """Annualized yield rate should be computed correctly."""
        historical = [1.0, 2.0, 3.0]
        forecast = forecast_drip_yield(historical, forecast_periods=2)
        assert forecast.annualized_yield_rate > 0

    def test_empty_yields_raises(self):
        """Empty historical yields should raise ValueError."""
        with pytest.raises(ValueError):
            forecast_drip_yield([])
