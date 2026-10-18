using System.Globalization;

namespace Tessera.SDK;

/// <summary>Filters for <c>GET /v1/assets</c>.</summary>
public sealed record ListAssetsQuery
{
    public string? AssetType { get; init; }
    public bool? Active { get; init; }
    public int? Offset { get; init; }
    public int? Limit { get; init; }

    /// <summary>Comma-separated sparse fieldset, e.g. <c>id,name,symbol</c>.</summary>
    public string? Fields { get; init; }

    internal QueryString ToQuery() => new QueryString()
        .Add("asset_type", AssetType)
        .Add("active", Active)
        .Add("offset", Offset)
        .Add("limit", Limit)
        .Add("fields", Fields);
}

/// <summary>Offset/limit pagination.</summary>
public sealed record PageQuery
{
    public int? Offset { get; init; }
    public int? Limit { get; init; }

    internal QueryString ToQuery() => new QueryString().Add("offset", Offset).Add("limit", Limit);
}

/// <summary>Optional window for portfolio performance.</summary>
public sealed record PortfolioQuery
{
    public DateTimeOffset? StartDate { get; init; }
    public DateTimeOffset? EndDate { get; init; }
    public double? DiscountRate { get; init; }

    internal QueryString ToQuery() => new QueryString()
        .Add("start_date", StartDate?.ToString("yyyy-MM-ddTHH:mm:ssK", CultureInfo.InvariantCulture))
        .Add("end_date", EndDate?.ToString("yyyy-MM-ddTHH:mm:ssK", CultureInfo.InvariantCulture))
        .Add("discount_rate", DiscountRate);
}

/// <summary>Window controls for <c>GET /v1/assets/{id}/metrics/analytics</c>.</summary>
public sealed record AnalyticsWindowQuery
{
    /// <summary>Window length, e.g. <c>1h</c> or <c>24h</c>.</summary>
    public string? Window { get; init; }

    /// <summary><c>sliding</c> or <c>tumbling</c>.</summary>
    public string? WindowType { get; init; }

    public int? Limit { get; init; }

    internal QueryString ToQuery() => new QueryString()
        .Add("window", Window)
        .Add("window_type", WindowType)
        .Add("limit", Limit);
}

/// <summary>Cap-table export formats supported by <c>GET /v1/assets/{id}/export</c>.</summary>
public enum ExportFormat
{
    Csv,
    Parquet,
}

/// <summary>Order-preserving query-string builder that skips unset values.</summary>
internal sealed class QueryString
{
    private readonly List<KeyValuePair<string, string>> _pairs = new();

    public QueryString Add(string key, string? value)
    {
        if (!string.IsNullOrEmpty(value))
        {
            _pairs.Add(new(key, value));
        }
        return this;
    }

    public QueryString Add(string key, int? value) =>
        value is null ? this : Add(key, value.Value.ToString(CultureInfo.InvariantCulture));

    public QueryString Add(string key, ulong? value) =>
        value is null ? this : Add(key, value.Value.ToString(CultureInfo.InvariantCulture));

    public QueryString Add(string key, double? value) =>
        value is null ? this : Add(key, value.Value.ToString("R", CultureInfo.InvariantCulture));

    public QueryString Add(string key, bool? value) =>
        value is null ? this : Add(key, value.Value ? "true" : "false");

    public override string ToString() => _pairs.Count == 0
        ? string.Empty
        : "?" + string.Join("&", _pairs.Select(p => $"{Uri.EscapeDataString(p.Key)}={Uri.EscapeDataString(p.Value)}"));
}
