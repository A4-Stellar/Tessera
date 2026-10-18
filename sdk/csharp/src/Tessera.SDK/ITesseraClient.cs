using System.Text.Json;

namespace Tessera.SDK;

/// <summary>
/// Typed client for the Tessera REST API and event stream. Register it with
/// <see cref="TesseraServiceCollectionExtensions.AddTesseraClient(Microsoft.Extensions.DependencyInjection.IServiceCollection, Action{TesseraClientOptions})"/>
/// to get a pooled <see cref="HttpClient"/> from <c>IHttpClientFactory</c>.
/// </summary>
public interface ITesseraClient
{
    /// <summary><c>GET /v1/stats</c> — platform-wide metrics and TVL.</summary>
    Task<Stats> GetStatsAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets</c> — indexed assets, optionally filtered.</summary>
    Task<IReadOnlyList<Asset>> ListAssetsAsync(ListAssetsQuery? query = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}</c>, optionally restricted to a sparse fieldset.</summary>
    Task<Asset> GetAssetAsync(ulong assetId, string? fields = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/holders</c> — holders sorted by balance descending.</summary>
    Task<IReadOnlyList<Holder>> GetAssetHoldersAsync(ulong assetId, PageQuery? page = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/compliance</c> — allowlist summary.</summary>
    Task<ComplianceSummary> GetAssetComplianceAsync(ulong assetId, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/dividends</c> — every distribution for the asset.</summary>
    Task<IReadOnlyList<Distribution>> GetAssetDividendsAsync(ulong assetId, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/distributions/{did}</c>.</summary>
    Task<Distribution> GetAssetDistributionAsync(ulong assetId, ulong distributionId, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/analytics</c> — Gini, HHI and velocity.</summary>
    Task<AssetAnalytics> GetAssetAnalyticsAsync(ulong assetId, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/metrics/analytics</c> — windowed analytics, returned as raw JSON.</summary>
    Task<JsonElement> GetAssetMetricsAnalyticsAsync(ulong assetId, AnalyticsWindowQuery? query = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/events</c> — diagnostic events.</summary>
    Task<AssetEvents> GetAssetEventsAsync(ulong assetId, bool includeDiagnostics = false, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/assets/{id}/export</c> — cap-table snapshot bytes.</summary>
    Task<byte[]> ExportAssetAsync(ulong assetId, ExportFormat format = ExportFormat.Csv, uint? ledger = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/holders/{address}</c> — every asset the address holds.</summary>
    Task<IReadOnlyList<AddressHolding>> GetHolderAsync(string address, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/holders/{address}/compliance</c>.</summary>
    Task<IReadOnlyList<AddressCompliance>> GetHolderComplianceAsync(string address, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/holders/{address}/portfolio</c> — TWR, NPV, IRR and time series.</summary>
    Task<HolderPortfolio> GetHolderPortfolioAsync(string address, PortfolioQuery? query = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/compliance/{address}</c>.</summary>
    Task<IReadOnlyList<AddressCompliance>> GetComplianceAsync(string address, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/events</c> — recent indexed contract events.</summary>
    Task<IReadOnlyList<TesseraEvent>> ListEventsAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/security/anomalies</c>.</summary>
    Task<AnomaliesResponse> GetSecurityAnomaliesAsync(string? account = null, double? minZScore = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/audit/verify</c> — verify the append-only audit log.</summary>
    Task<AuditVerificationResult> VerifyAuditAsync(ulong? entrySequence = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/audit/entries</c>.</summary>
    Task<AuditEntriesPage> ListAuditEntriesAsync(PageQuery? page = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/audit/entries/{sequence}</c> — entry plus inclusion proof.</summary>
    Task<AuditEntryDetail> GetAuditEntryAsync(ulong sequence, CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/audit/anchors</c>.</summary>
    Task<IReadOnlyList<AnchorRecord>> ListAuditAnchorsAsync(CancellationToken cancellationToken = default);

    /// <summary><c>POST /v1/audit/entries</c>. Not retried: the call is not idempotent.</summary>
    Task<AuditEntry> CreateAuditEntryAsync(CreateAuditEntryRequest request, CancellationToken cancellationToken = default);

    /// <summary><c>POST /v1/audit/anchor</c>. Not retried: the call is not idempotent.</summary>
    Task<AnchorRecord> PublishAuditAnchorAsync(PublishAnchorRequest? request = null, CancellationToken cancellationToken = default);

    /// <summary><c>POST /v1/admin/dlq/retry</c>.</summary>
    Task<RetryReport> RetryDeadLetterQueueAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/proofs/mmr</c> — current MMR checkpoint root, as raw JSON.</summary>
    Task<JsonElement> GetMmrRootAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /v1/proofs/mmr/{ledger}</c> — light-client proof for a ledger, as raw JSON.</summary>
    Task<JsonElement> GetMmrProofAsync(uint ledgerSequence, string? account = null, CancellationToken cancellationToken = default);

    /// <summary><c>GET /</c> — service metadata.</summary>
    Task<JsonElement> GetIndexAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /version</c>.</summary>
    Task<JsonElement> GetVersionAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /health</c>.</summary>
    Task<JsonElement> GetHealthAsync(CancellationToken cancellationToken = default);

    /// <summary><c>GET /metrics</c> — Prometheus exposition text (requires the auth token).</summary>
    Task<string> GetMetricsAsync(CancellationToken cancellationToken = default);

    /// <summary>
    /// Opens the <c>/v1/ws</c> event stream, subscribes to <paramref name="topics"/> and yields
    /// events as they arrive. A topic matches an event when it occurs (case-insensitively) in the
    /// event type, contract ID or payload. The connection is closed when enumeration stops or
    /// <paramref name="cancellationToken"/> is cancelled.
    /// </summary>
    IAsyncEnumerable<TesseraEvent> StreamEventsAsync(IEnumerable<string> topics, CancellationToken cancellationToken = default);
}
