using System.Globalization;
using System.Net;
using System.Net.Http.Headers;
using System.Text.Json;
using Microsoft.Extensions.Options;

namespace Tessera.SDK;

/// <summary>
/// Default <see cref="ITesseraClient"/> implementation. It is safe to share across threads.
/// Prefer resolving it from DI (<c>AddTesseraClient</c>) so the <see cref="HttpClient"/> comes
/// from <c>IHttpClientFactory</c>; use <see cref="Create(TesseraClientOptions)"/> elsewhere.
/// </summary>
public sealed partial class TesseraClient : ITesseraClient, IDisposable
{
    internal const string UserAgent = "tessera-dotnet/0.1.0";

    private readonly HttpClient _http;
    private readonly TesseraClientOptions _options;
    private readonly bool _ownsHttpClient;

    /// <summary>Constructor used by <c>IHttpClientFactory</c> typed-client activation.</summary>
    public TesseraClient(HttpClient httpClient, IOptions<TesseraClientOptions> options)
        : this(httpClient, options.Value, ownsHttpClient: false)
    {
    }

    private TesseraClient(HttpClient httpClient, TesseraClientOptions options, bool ownsHttpClient)
    {
        ArgumentNullException.ThrowIfNull(httpClient);
        ArgumentNullException.ThrowIfNull(options);
        options.Validate();
        _http = httpClient;
        _options = options;
        _ownsHttpClient = ownsHttpClient;
    }

    /// <summary>
    /// Creates a standalone client that owns a pooled <see cref="SocketsHttpHandler"/>
    /// (connections are recycled every few minutes so DNS changes are picked up).
    /// </summary>
    public static TesseraClient Create(TesseraClientOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);
        var handler = new SocketsHttpHandler { PooledConnectionLifetime = TimeSpan.FromMinutes(5) };
        var http = new HttpClient(handler, disposeHandler: true);
        TesseraServiceCollectionExtensions.ConfigureHttpClient(http, options);
        return new TesseraClient(http, options, ownsHttpClient: true);
    }

    /// <summary>Creates a client around a caller-managed <see cref="HttpClient"/>.</summary>
    public static TesseraClient Create(HttpClient httpClient, TesseraClientOptions options) =>
        new(httpClient, options, ownsHttpClient: false);

    /// <inheritdoc />
    public void Dispose()
    {
        if (_ownsHttpClient)
        {
            _http.Dispose();
        }
    }

    // ── Endpoints ──────────────────────────────────────────────────────────

    /// <inheritdoc />
    public Task<Stats> GetStatsAsync(CancellationToken cancellationToken = default) =>
        GetJsonAsync<Stats>("/v1/stats", cancellationToken);

    /// <inheritdoc />
    public async Task<IReadOnlyList<Asset>> ListAssetsAsync(ListAssetsQuery? query = null, CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<Asset>>("/v1/assets" + (query?.ToQuery().ToString() ?? string.Empty), cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public Task<Asset> GetAssetAsync(ulong assetId, string? fields = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<Asset>($"/v1/assets/{Id(assetId)}{new QueryString().Add("fields", fields)}", cancellationToken);

    /// <inheritdoc />
    public async Task<IReadOnlyList<Holder>> GetAssetHoldersAsync(ulong assetId, PageQuery? page = null, CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<Holder>>($"/v1/assets/{Id(assetId)}/holders{page?.ToQuery()}", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public Task<ComplianceSummary> GetAssetComplianceAsync(ulong assetId, CancellationToken cancellationToken = default) =>
        GetJsonAsync<ComplianceSummary>($"/v1/assets/{Id(assetId)}/compliance", cancellationToken);

    /// <inheritdoc />
    public async Task<IReadOnlyList<Distribution>> GetAssetDividendsAsync(ulong assetId, CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<Distribution>>($"/v1/assets/{Id(assetId)}/dividends", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public Task<Distribution> GetAssetDistributionAsync(ulong assetId, ulong distributionId, CancellationToken cancellationToken = default) =>
        GetJsonAsync<Distribution>($"/v1/assets/{Id(assetId)}/distributions/{Id(distributionId)}", cancellationToken);

    /// <inheritdoc />
    public Task<AssetAnalytics> GetAssetAnalyticsAsync(ulong assetId, CancellationToken cancellationToken = default) =>
        GetJsonAsync<AssetAnalytics>($"/v1/assets/{Id(assetId)}/analytics", cancellationToken);

    /// <inheritdoc />
    public Task<JsonElement> GetAssetMetricsAnalyticsAsync(ulong assetId, AnalyticsWindowQuery? query = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<JsonElement>($"/v1/assets/{Id(assetId)}/metrics/analytics{query?.ToQuery()}", cancellationToken);

    /// <inheritdoc />
    public Task<AssetEvents> GetAssetEventsAsync(ulong assetId, bool includeDiagnostics = false, CancellationToken cancellationToken = default) =>
        GetJsonAsync<AssetEvents>($"/v1/assets/{Id(assetId)}/events{new QueryString().Add("include_diagnostics", includeDiagnostics ? true : null)}", cancellationToken);

    /// <inheritdoc />
    public async Task<byte[]> ExportAssetAsync(ulong assetId, ExportFormat format = ExportFormat.Csv, uint? ledger = null, CancellationToken cancellationToken = default)
    {
        var query = new QueryString()
            .Add("format", format == ExportFormat.Parquet ? "parquet" : "csv")
            .Add("ledger", ledger);
        using var response = await SendAsync(HttpMethod.Get, $"/v1/assets/{Id(assetId)}/export{query}", body: null, cancellationToken).ConfigureAwait(false);
        return await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<AddressHolding>> GetHolderAsync(string address, CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<AddressHolding>>($"/v1/holders/{Segment(address)}", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<IReadOnlyList<AddressCompliance>> GetHolderComplianceAsync(string address, CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<AddressCompliance>>($"/v1/holders/{Segment(address)}/compliance", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public Task<HolderPortfolio> GetHolderPortfolioAsync(string address, PortfolioQuery? query = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<HolderPortfolio>($"/v1/holders/{Segment(address)}/portfolio{query?.ToQuery()}", cancellationToken);

    /// <inheritdoc />
    public async Task<IReadOnlyList<AddressCompliance>> GetComplianceAsync(string address, CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<AddressCompliance>>($"/v1/compliance/{Segment(address)}", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<IReadOnlyList<TesseraEvent>> ListEventsAsync(CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<TesseraEvent>>("/v1/events", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public Task<AnomaliesResponse> GetSecurityAnomaliesAsync(string? account = null, double? minZScore = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<AnomaliesResponse>($"/v1/security/anomalies{new QueryString().Add("account", account).Add("min_z", minZScore)}", cancellationToken);

    /// <inheritdoc />
    public Task<AuditVerificationResult> VerifyAuditAsync(ulong? entrySequence = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<AuditVerificationResult>($"/v1/audit/verify{new QueryString().Add("entry_seq", entrySequence)}", cancellationToken);

    /// <inheritdoc />
    public Task<AuditEntriesPage> ListAuditEntriesAsync(PageQuery? page = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<AuditEntriesPage>($"/v1/audit/entries{page?.ToQuery()}", cancellationToken);

    /// <inheritdoc />
    public Task<AuditEntryDetail> GetAuditEntryAsync(ulong sequence, CancellationToken cancellationToken = default) =>
        GetJsonAsync<AuditEntryDetail>($"/v1/audit/entries/{Id(sequence)}", cancellationToken);

    /// <inheritdoc />
    public async Task<IReadOnlyList<AnchorRecord>> ListAuditAnchorsAsync(CancellationToken cancellationToken = default) =>
        await GetJsonAsync<List<AnchorRecord>>("/v1/audit/anchors", cancellationToken).ConfigureAwait(false);

    /// <inheritdoc />
    public Task<AuditEntry> CreateAuditEntryAsync(CreateAuditEntryRequest request, CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);
        return SendJsonAsync<AuditEntry>(HttpMethod.Post, "/v1/audit/entries", request, cancellationToken);
    }

    /// <inheritdoc />
    public Task<AnchorRecord> PublishAuditAnchorAsync(PublishAnchorRequest? request = null, CancellationToken cancellationToken = default) =>
        SendJsonAsync<AnchorRecord>(HttpMethod.Post, "/v1/audit/anchor", request ?? new PublishAnchorRequest(), cancellationToken);

    /// <inheritdoc />
    public Task<RetryReport> RetryDeadLetterQueueAsync(CancellationToken cancellationToken = default) =>
        SendJsonAsync<RetryReport>(HttpMethod.Post, "/v1/admin/dlq/retry", null, cancellationToken);

    /// <inheritdoc />
    public Task<JsonElement> GetMmrRootAsync(CancellationToken cancellationToken = default) =>
        GetJsonAsync<JsonElement>("/v1/proofs/mmr", cancellationToken);

    /// <inheritdoc />
    public Task<JsonElement> GetMmrProofAsync(uint ledgerSequence, string? account = null, CancellationToken cancellationToken = default) =>
        GetJsonAsync<JsonElement>($"/v1/proofs/mmr/{ledgerSequence.ToString(CultureInfo.InvariantCulture)}{new QueryString().Add("account", account)}", cancellationToken);

    /// <inheritdoc />
    public Task<JsonElement> GetIndexAsync(CancellationToken cancellationToken = default) => GetJsonAsync<JsonElement>("/", cancellationToken);

    /// <inheritdoc />
    public Task<JsonElement> GetVersionAsync(CancellationToken cancellationToken = default) => GetJsonAsync<JsonElement>("/version", cancellationToken);

    /// <inheritdoc />
    public Task<JsonElement> GetHealthAsync(CancellationToken cancellationToken = default) => GetJsonAsync<JsonElement>("/health", cancellationToken);

    /// <inheritdoc />
    public async Task<string> GetMetricsAsync(CancellationToken cancellationToken = default)
    {
        using var response = await SendAsync(HttpMethod.Get, "/metrics", body: null, cancellationToken).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
    }

    // ── Transport ──────────────────────────────────────────────────────────

    private static string Id(ulong value) => value.ToString(CultureInfo.InvariantCulture);

    private static string Segment(string value)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(value);
        return Uri.EscapeDataString(value);
    }

    private Task<T> GetJsonAsync<T>(string pathAndQuery, CancellationToken cancellationToken) =>
        SendJsonAsync<T>(HttpMethod.Get, pathAndQuery, null, cancellationToken);

    private async Task<T> SendJsonAsync<T>(HttpMethod method, string pathAndQuery, object? body, CancellationToken cancellationToken)
    {
        byte[]? payload = body is null ? null : JsonSerializer.SerializeToUtf8Bytes(body, body.GetType(), TesseraJson.Options);
        using var response = await SendAsync(method, pathAndQuery, payload, cancellationToken).ConfigureAwait(false);
        await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            var result = await JsonSerializer.DeserializeAsync<T>(stream, TesseraJson.Options, cancellationToken).ConfigureAwait(false);
            return result ?? throw new TesseraSerializationException($"{method} {pathAndQuery} returned an empty body.", new JsonException("null"));
        }
        catch (JsonException ex)
        {
            throw new TesseraSerializationException($"Could not decode the response of {method} {pathAndQuery} as {typeof(T).Name}.", ex);
        }
    }

    /// <summary>
    /// Sends a request with the SDK retry policy: idempotent methods are retried on network
    /// failures and 5xx responses; any method is retried on 429, honouring <c>Retry-After</c>.
    /// Delays grow exponentially from <see cref="TesseraClientOptions.InitialBackoff"/> with jitter.
    /// </summary>
    private async Task<HttpResponseMessage> SendAsync(HttpMethod method, string pathAndQuery, byte[]? body, CancellationToken cancellationToken)
    {
        var uri = new Uri(_options.BaseAddress, pathAndQuery);
        var operation = $"{method} {pathAndQuery}";
        var idempotent = method == HttpMethod.Get || method == HttpMethod.Head || method == HttpMethod.Options;
        var backoff = _options.InitialBackoff;

        for (var attempt = 0; ; attempt++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var canRetry = attempt < _options.MaxRetries;

            using var request = new HttpRequestMessage(method, uri);
            request.Headers.UserAgent.ParseAdd(UserAgent);
            request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
            if (!string.IsNullOrEmpty(_options.AuthToken))
            {
                request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _options.AuthToken);
            }
            foreach (var (name, value) in _options.CustomHeaders)
            {
                request.Headers.Remove(name);
                request.Headers.TryAddWithoutValidation(name, value);
            }
            if (body is not null)
            {
                request.Content = new ByteArrayContent(body);
                request.Content.Headers.ContentType = new MediaTypeHeaderValue("application/json") { CharSet = "utf-8" };
            }

            HttpResponseMessage response;
            try
            {
                response = await _http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
            }
            catch (Exception ex) when (ex is HttpRequestException || (ex is TaskCanceledException && !cancellationToken.IsCancellationRequested))
            {
                // A TaskCanceledException without caller cancellation is the HttpClient timeout.
                if (idempotent && canRetry)
                {
                    await DelayAsync(ref backoff, null, cancellationToken).ConfigureAwait(false);
                    continue;
                }
                throw new TesseraNetworkException(operation, ex);
            }

            if (response.IsSuccessStatusCode)
            {
                return response;
            }

            var status = response.StatusCode;
            var retryAfter = RetryAfter(response);
            if ((status == HttpStatusCode.TooManyRequests || ((int)status >= 500 && idempotent)) && canRetry)
            {
                response.Dispose();
                await DelayAsync(ref backoff, status == HttpStatusCode.TooManyRequests ? retryAfter : null, cancellationToken).ConfigureAwait(false);
                continue;
            }

            using (response)
            {
                var text = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
                if (status == HttpStatusCode.TooManyRequests)
                {
                    throw new TesseraRateLimitException(retryAfter, text);
                }
                throw ToApiException(status, text);
            }
        }
    }

    private static TesseraApiException ToApiException(HttpStatusCode status, string text)
    {
        try
        {
            var body = JsonSerializer.Deserialize<ApiErrorBody>(text, TesseraJson.Options);
            if (!string.IsNullOrEmpty(body?.Error))
            {
                return new TesseraApiException(status, body.Error, body.Message ?? string.Empty);
            }
        }
        catch (JsonException)
        {
            // Non-JSON error body (e.g. a proxy page): fall through to the raw text.
        }
        return new TesseraApiException(status, status.ToString(), text);
    }

    private static TimeSpan? RetryAfter(HttpResponseMessage response)
    {
        var header = response.Headers.RetryAfter;
        if (header?.Delta is { } delta && delta > TimeSpan.Zero)
        {
            return delta;
        }
        if (header?.Date is { } date)
        {
            var wait = date - DateTimeOffset.UtcNow;
            return wait > TimeSpan.Zero ? wait : null;
        }
        return null;
    }

    private Task DelayAsync(ref TimeSpan backoff, TimeSpan? retryAfter, CancellationToken cancellationToken)
    {
        var delay = retryAfter ?? WithJitter(backoff);
        var doubled = TimeSpan.FromTicks(Math.Min(backoff.Ticks * 2, _options.MaxBackoff.Ticks));
        backoff = doubled;
        return delay <= TimeSpan.Zero ? Task.CompletedTask : Task.Delay(delay, cancellationToken);
    }

    private static TimeSpan WithJitter(TimeSpan delay)
    {
        var quarter = delay.Ticks / 4;
        return quarter <= 0 ? delay : delay + TimeSpan.FromTicks(Random.Shared.NextInt64(quarter));
    }
}
