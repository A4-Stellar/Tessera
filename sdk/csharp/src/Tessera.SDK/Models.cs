using System.Text.Json;
using System.Text.Json.Serialization;

namespace Tessera.SDK;

// Response models for the Tessera REST API (api/src/models). Properties map to the
// snake_case wire format through TesseraJson.Options; explicit names are only
// given where the naming policy would not produce the server's key. Token
// amounts and cent values are strings on the wire to keep i128 precision.

/// <summary>Platform-wide aggregated metrics (<c>GET /v1/stats</c>).</summary>
public sealed record Stats
{
    public int TotalAssets { get; init; }
    public int ActiveAssets { get; init; }
    public string TvlCents { get; init; } = "0";
    public double TvlUsd { get; init; }
    public int TotalHolders { get; init; }
    public int TotalDistributions { get; init; }
    public uint LastIndexedLedger { get; init; }
    public string? LastUpdated { get; init; }
}

/// <summary>A tokenized real-world asset indexed from Stellar.</summary>
public sealed record Asset
{
    public ulong Id { get; init; }
    public string TokenContract { get; init; } = string.Empty;
    public string Issuer { get; init; } = string.Empty;
    public string Name { get; init; } = string.Empty;
    public string Symbol { get; init; } = string.Empty;
    public string AssetType { get; init; } = string.Empty;
    public string Description { get; init; } = string.Empty;
    public string ValuationCents { get; init; } = "0";
    public double ValuationUsd { get; init; }
    public uint Decimals { get; init; }
    public string TotalSupply { get; init; } = "0";
    public int Holders { get; init; }
    public bool Active { get; init; }
    public bool Paused { get; init; }
    public string ComplianceContract { get; init; } = string.Empty;
    public uint CreatedAtLedger { get; init; }
    public uint IndexedAtLedger { get; init; }
    public string? IndexError { get; init; }
}

/// <summary>A holder of an asset, sorted by balance descending in list responses.</summary>
public sealed record Holder
{
    public string Address { get; init; } = string.Empty;
    public string Balance { get; init; } = "0";
    public double SharePercent { get; init; }
}

/// <summary>One asset held by an investor address.</summary>
public sealed record AddressHolding
{
    public string Address { get; init; } = string.Empty;
    public ulong AssetId { get; init; }
    public string AssetName { get; init; } = string.Empty;
    public string Symbol { get; init; } = string.Empty;
    public string Balance { get; init; } = "0";
    public double SharePercent { get; init; }
}

/// <summary>Compliance status of an address within one asset.</summary>
public sealed record AddressCompliance
{
    public string Address { get; init; } = string.Empty;
    public ulong AssetId { get; init; }
    public string AssetName { get; init; } = string.Empty;
    public string Symbol { get; init; } = string.Empty;
    public string Balance { get; init; } = "0";
    public string Status { get; init; } = string.Empty;
    public bool Allowed { get; init; }
}

/// <summary>Aggregate allowlist summary for an asset.</summary>
public sealed record ComplianceSummary
{
    public int TotalRecords { get; init; }
    public int Approved { get; init; }
    public int Suspended { get; init; }
    public int Rejected { get; init; }
    public int Pending { get; init; }
    public int WithExpiry { get; init; }
    public IReadOnlyList<JurisdictionCount> Jurisdictions { get; init; } = Array.Empty<JurisdictionCount>();
}

/// <summary>Number of compliance records in one jurisdiction.</summary>
public sealed record JurisdictionCount
{
    public string Jurisdiction { get; init; } = string.Empty;
    public int Count { get; init; }
}

/// <summary>A dividend distribution pool.</summary>
public sealed record Distribution
{
    public ulong Id { get; init; }
    public string AssetToken { get; init; } = string.Empty;
    public string PaymentToken { get; init; } = string.Empty;
    public string TotalAmount { get; init; } = "0";
    public string Distributed { get; init; } = "0";
    public double ClaimedPercent { get; init; }
    public bool OverflowDetected { get; init; }
    public bool Completed { get; init; }
    public uint CreatedAtLedger { get; init; }
    public double? FiatEquivalentUsd { get; init; }
}

/// <summary>An indexed contract event, as returned by REST and pushed over the WebSocket.</summary>
public sealed record TesseraEvent
{
    public ulong Id { get; init; }
    public string Contract { get; init; } = string.Empty;
    public string EventType { get; init; } = string.Empty;
    public uint Ledger { get; init; }
    public string? Timestamp { get; init; }

    /// <summary>Event payload; its shape depends on <see cref="EventType"/>.</summary>
    public JsonElement Data { get; init; }
}

/// <summary>Holder concentration and transaction velocity for an asset.</summary>
public sealed record AssetAnalytics
{
    public ulong AssetId { get; init; }
    public int HolderCount { get; init; }
    public double? GiniCoefficient { get; init; }
    public double? HerfindahlHirschmanIndex { get; init; }

    [JsonPropertyName("volume_24h")]
    public string Volume24h { get; init; } = "0";

    public string TotalSupply { get; init; } = "0";

    [JsonPropertyName("velocity_24h")]
    public double? Velocity24h { get; init; }

    public string WindowStart { get; init; } = string.Empty;
    public string WindowEnd { get; init; } = string.Empty;
}

/// <summary>Portfolio analytics for an investor address.</summary>
public sealed record HolderPortfolio
{
    public string Address { get; init; } = string.Empty;
    public PortfolioSummary Summary { get; init; } = new();
    public IReadOnlyList<PortfolioHolding> Holdings { get; init; } = Array.Empty<PortfolioHolding>();
    public IReadOnlyList<PortfolioDividendPayout> Dividends { get; init; } = Array.Empty<PortfolioDividendPayout>();
    public PortfolioPerformance Performance { get; init; } = new();
    public IReadOnlyList<PortfolioTimeSeriesPoint> TimeSeries { get; init; } = Array.Empty<PortfolioTimeSeriesPoint>();
}

public sealed record PortfolioSummary
{
    public double TotalValuationUsd { get; init; }
    public int TotalAssetsHeld { get; init; }
    public double TotalCashInflowsUsd { get; init; }
    public double TotalCashOutflowsUsd { get; init; }
    public double NetInvestedCapitalUsd { get; init; }
    public double TotalDividendsUsd { get; init; }
    public double TotalUnrealizedGainUsd { get; init; }
}

public sealed record PortfolioHolding
{
    public ulong AssetId { get; init; }
    public string AssetName { get; init; } = string.Empty;
    public string Symbol { get; init; } = string.Empty;
    public string AssetType { get; init; } = string.Empty;
    public string Balance { get; init; } = "0";
    public uint Decimals { get; init; }
    public double SharePercent { get; init; }
    public double UnitPriceUsd { get; init; }
    public double MarketValueUsd { get; init; }
    public double DividendsReceivedUsd { get; init; }
}

public sealed record PortfolioDividendPayout
{
    public ulong DistributionId { get; init; }
    public ulong AssetId { get; init; }
    public string PaymentToken { get; init; } = string.Empty;
    public string Amount { get; init; } = "0";
    public double AmountUsd { get; init; }
    public uint ClaimedAtLedger { get; init; }
    public string? Timestamp { get; init; }
}

public sealed record PortfolioPerformance
{
    public double TimeWeightedReturn { get; init; }
    public double TimeWeightedReturnPct { get; init; }
    public double NetPresentValue { get; init; }
    public double? InternalRateOfReturn { get; init; }
    public double? InternalRateOfReturnPct { get; init; }
    public double DiscountRate { get; init; }
}

public sealed record PortfolioTimeSeriesPoint
{
    public string Timestamp { get; init; } = string.Empty;
    public uint Ledger { get; init; }
    public double PortfolioValueUsd { get; init; }
    public double CashFlowUsd { get; init; }
    public double CumulativeInvestedUsd { get; init; }
    public double CumulativeDividendsUsd { get; init; }
    public double PeriodReturn { get; init; }
    public double CumulativeTwrPct { get; init; }
}

/// <summary>Diagnostic events captured for an asset.</summary>
public sealed record AssetEvents
{
    public ulong AssetId { get; init; }
    public IReadOnlyList<DiagnosticEvent> Diagnostics { get; init; } = Array.Empty<DiagnosticEvent>();
}

public sealed record DiagnosticEvent
{
    public string? Contract { get; init; }
    public string EventType { get; init; } = string.Empty;
    public IReadOnlyList<JsonElement> Topics { get; init; } = Array.Empty<JsonElement>();
    public JsonElement Data { get; init; }
    public bool InSuccessfulContractCall { get; init; }
    public uint? ErrorCode { get; init; }
}

/// <summary>Active anomaly flags (<c>GET /v1/security/anomalies</c>).</summary>
public sealed record AnomaliesResponse
{
    public double Threshold { get; init; }
    public int Count { get; init; }
    public IReadOnlyList<RiskFlag> Flags { get; init; } = Array.Empty<RiskFlag>();
}

public sealed record RiskFlag
{
    public string Account { get; init; } = string.Empty;
    public string Metric { get; init; } = string.Empty;
    public double PeakZScore { get; init; }
    public double LatestZScore { get; init; }
    public double Observed { get; init; }
    public double BaselineMean { get; init; }
    public double BaselineStd { get; init; }
    public ulong FirstSeen { get; init; }
    public ulong LastSeen { get; init; }
    public ulong TriggerCount { get; init; }
}

/// <summary>A page of audit log entries.</summary>
public sealed record AuditEntriesPage
{
    public IReadOnlyList<AuditEntry> Entries { get; init; } = Array.Empty<AuditEntry>();
    public int Total { get; init; }
    public int Offset { get; init; }
    public int Limit { get; init; }
}

public sealed record AuditEntry
{
    public ulong Sequence { get; init; }
    public DateTimeOffset Timestamp { get; init; }
    public JsonElement Action { get; init; }
    public string Actor { get; init; } = string.Empty;
    public string? Target { get; init; }
    public JsonElement Payload { get; init; }
    public string PrevHash { get; init; } = string.Empty;
    public string EntryHash { get; init; } = string.Empty;
}

/// <summary>An audit entry with its Merkle inclusion proof.</summary>
public sealed record AuditEntryDetail
{
    public AuditEntry Entry { get; init; } = new();
    public MerkleProof? InclusionProof { get; init; }
}

public sealed record MerkleProof
{
    public ulong LeafIndex { get; init; }
    public string LeafHash { get; init; } = string.Empty;
    public IReadOnlyList<ProofStep> Steps { get; init; } = Array.Empty<ProofStep>();
    public string RootHash { get; init; } = string.Empty;
}

public sealed record ProofStep
{
    public string SiblingHash { get; init; } = string.Empty;
    public bool IsSiblingLeft { get; init; }
}

/// <summary>An audit root published to the Stellar ledger.</summary>
public sealed record AnchorRecord
{
    public ulong AnchorId { get; init; }
    public string RootHash { get; init; } = string.Empty;
    public ulong StartSequence { get; init; }
    public ulong EndSequence { get; init; }
    public int EntryCount { get; init; }
    public uint LedgerSequence { get; init; }
    public string MemoHash { get; init; } = string.Empty;
    public string? ContractStorageKey { get; init; }
    public string TxHash { get; init; } = string.Empty;
    public DateTimeOffset PublishedAt { get; init; }
}

public sealed record AuditVerificationResult
{
    public bool IsValid { get; init; }
    public int TotalEntries { get; init; }
    public string GenesisHash { get; init; } = string.Empty;
    public string ChainTipHash { get; init; } = string.Empty;
    public string MerkleRoot { get; init; } = string.Empty;
    public string? AnchoredRootHash { get; init; }
    public string AnchorStatus { get; init; } = string.Empty;
    public AnchorRecord? LatestAnchor { get; init; }
    public ulong? FirstTamperIndex { get; init; }
    public string? ErrorDetail { get; init; }
}

/// <summary>Request body for <c>POST /v1/audit/entries</c>.</summary>
public sealed record CreateAuditEntryRequest
{
    public required JsonElement Action { get; init; }
    public required string Actor { get; init; }
    public string? Target { get; init; }
    public JsonElement? Payload { get; init; }
}

/// <summary>Request body for <c>POST /v1/audit/anchor</c>.</summary>
public sealed record PublishAnchorRequest
{
    public uint? LedgerSequence { get; init; }
}

/// <summary>Outcome of <c>POST /v1/admin/dlq/retry</c>.</summary>
public sealed record RetryReport
{
    public int Retried { get; init; }
    public int Resolved { get; init; }
    public int StillQuarantined { get; init; }
}

internal sealed record ApiErrorBody
{
    public string? Error { get; init; }
    public string? Message { get; init; }
}
