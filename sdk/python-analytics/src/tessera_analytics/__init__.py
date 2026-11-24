from .analytics import (
    calculate_gini_coefficient,
    compute_holder_retention,
    forecast_drip_yield,
)
from .deprecation import (
    parse_successor,
    request_with_deprecation_warning,
    warn_if_deprecated,
)
from .kafka_events import (
    AvroBinaryDecoder,
    CachedSchemaRegistryClient,
    KafkaEventSource,
    SchemaRegistryClient,
)
from .export import (
    export_to_notebook,
    plot_capitalization_curve,
    plot_drip_yield_forecast,
    plot_holder_distribution,
)
from .models import (
    CapTableAnalysis,
    DripYieldForecast,
    HolderMetrics,
    RetentionMetrics,
)

__version__ = "0.1.0"

__all__ = [
    "calculate_gini_coefficient",
    "compute_holder_retention",
    "forecast_drip_yield",
    "export_to_notebook",
    "plot_capitalization_curve",
    "plot_drip_yield_forecast",
    "plot_holder_distribution",
    "CapTableAnalysis",
    "DripYieldForecast",
    "HolderMetrics",
    "RetentionMetrics",
    "parse_successor",
    "warn_if_deprecated",
    "request_with_deprecation_warning",
    "SchemaRegistryClient",
    "CachedSchemaRegistryClient",
    "AvroBinaryDecoder",
    "KafkaEventSource",
    "__version__",
]
