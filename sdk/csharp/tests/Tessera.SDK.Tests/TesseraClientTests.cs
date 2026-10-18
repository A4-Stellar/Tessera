using System.Net;
using System.Text.Json;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Options;
using Xunit;

namespace Tessera.SDK.Tests;

public class TesseraClientTests
{
    [Fact]
    public async Task GetStats_decodes_snake_case_and_sends_standard_headers()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, """
            {"total_assets":3,"active_assets":2,"tvl_cents":"123456789012345678901","tvl_usd":1234567.89,
             "total_holders":40,"total_distributions":5,"last_indexed_ledger":51234,"last_updated":null}
            """);
        using var client = Clients.With(handler, o =>
        {
            o.AuthToken = "secret";
            o.CustomHeaders["X-Tenant"] = "acme";
        });

        var stats = await client.GetStatsAsync();

        Assert.Equal(3, stats.TotalAssets);
        Assert.Equal("123456789012345678901", stats.TvlCents);
        Assert.Equal(51234u, stats.LastIndexedLedger);
        Assert.Null(stats.LastUpdated);

        var request = handler.Requests.Single().Request;
        Assert.Equal(HttpMethod.Get, request.Method);
        Assert.Equal("https://api.tessera.test/v1/stats", request.RequestUri!.ToString());
        Assert.Equal("Bearer secret", request.Headers.Authorization!.ToString());
        Assert.Equal("acme", request.Headers.GetValues("X-Tenant").Single());
        Assert.Contains("tessera-dotnet", request.Headers.UserAgent.ToString());
        Assert.Contains("application/json", request.Headers.Accept.ToString());
    }

    [Fact]
    public async Task ListAssets_builds_query_string_and_skips_unset_filters()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, """
            [{"id":7,"token_contract":"CTOKEN","issuer":"GISSUER","name":"Loft","symbol":"LOFT","asset_type":"real_estate",
              "description":"","valuation_cents":"500000000","valuation_usd":5000000,"decimals":7,"total_supply":"1000",
              "holders":12,"active":true,"paused":false,"compliance_contract":"CCOMP","created_at_ledger":1,"indexed_at_ledger":2}]
            """);
        using var client = Clients.With(handler);

        var assets = await client.ListAssetsAsync(new ListAssetsQuery { AssetType = "real_estate", Active = true, Limit = 10, Fields = "id,name" });

        var asset = Assert.Single(assets);
        Assert.Equal(7ul, asset.Id);
        Assert.Equal("real_estate", asset.AssetType);
        Assert.Null(asset.IndexError);
        Assert.Equal("/v1/assets?asset_type=real_estate&active=true&limit=10&fields=id%2Cname", handler.Requests[0].Request.RequestUri!.PathAndQuery);
    }

    [Fact]
    public async Task Analytics_maps_24h_fields_and_optional_metrics()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, """
            {"asset_id":1,"holder_count":4,"gini_coefficient":0.42,"herfindahl_hirschman_index":null,
             "volume_24h":"900","total_supply":"1000","velocity_24h":0.9,"window_start":"a","window_end":"b"}
            """);
        using var client = Clients.With(handler);

        var analytics = await client.GetAssetAnalyticsAsync(1);

        Assert.Equal(0.42, analytics.GiniCoefficient);
        Assert.Null(analytics.HerfindahlHirschmanIndex);
        Assert.Equal("900", analytics.Volume24h);
        Assert.Equal(0.9, analytics.Velocity24h);
    }

    [Fact]
    public async Task Address_segments_are_escaped()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, "[]");
        using var client = Clients.With(handler);

        await client.GetHolderComplianceAsync("G ABC/../x");

        Assert.Equal("/v1/holders/G%20ABC%2F..%2Fx/compliance", handler.Requests[0].Request.RequestUri!.AbsolutePath);
    }

    [Fact]
    public async Task Api_errors_surface_the_error_code_and_message()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.NotFound, """{"error":"not_found","message":"no asset with id 9"}""");
        using var client = Clients.With(handler);

        var error = await Assert.ThrowsAsync<TesseraApiException>(() => client.GetAssetAsync(9));

        Assert.Equal(HttpStatusCode.NotFound, error.StatusCode);
        Assert.Equal("not_found", error.ErrorCode);
        Assert.Equal("no asset with id 9", error.ApiMessage);
        Assert.Single(handler.Requests);
    }

    [Fact]
    public async Task Non_json_error_bodies_are_kept_verbatim()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.BadGateway, "<html>bad gateway</html>");
        using var client = Clients.With(handler, o => o.MaxRetries = 0);

        var error = await Assert.ThrowsAsync<TesseraApiException>(() => client.GetStatsAsync());

        Assert.Equal("BadGateway", error.ErrorCode);
        Assert.Equal("<html>bad gateway</html>", error.ApiMessage);
    }

    [Fact]
    public async Task Idempotent_requests_retry_server_errors()
    {
        var handler = new StubHandler()
            .Respond(HttpStatusCode.ServiceUnavailable, """{"error":"service_unavailable","message":"warming up"}""")
            .Respond(HttpStatusCode.InternalServerError, "oops")
            .Respond(HttpStatusCode.OK, "[]");
        using var client = Clients.With(handler);

        var events = await client.ListEventsAsync();

        Assert.Empty(events);
        Assert.Equal(3, handler.Requests.Count);
    }

    [Fact]
    public async Task Posts_are_not_retried_on_server_errors()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.InternalServerError, """{"error":"internal","message":"boom"}""");
        using var client = Clients.With(handler);

        await Assert.ThrowsAsync<TesseraApiException>(() => client.RetryDeadLetterQueueAsync());

        Assert.Single(handler.Requests);
    }

    [Fact]
    public async Task Rate_limits_retry_then_raise_with_retry_after()
    {
        static void RetryAfter(HttpResponseMessage r) => r.Headers.RetryAfter = new System.Net.Http.Headers.RetryConditionHeaderValue(TimeSpan.FromMilliseconds(1));
        var handler = new StubHandler()
            .Respond(HttpStatusCode.TooManyRequests, "slow down", RetryAfter)
            .Respond(HttpStatusCode.TooManyRequests, "slow down", RetryAfter);
        using var client = Clients.With(handler, o => o.MaxRetries = 1);

        var error = await Assert.ThrowsAsync<TesseraRateLimitException>(() => client.GetStatsAsync());

        Assert.Equal(2, handler.Requests.Count);
        Assert.Equal(TimeSpan.FromMilliseconds(1), error.RetryAfter);
        Assert.Equal(HttpStatusCode.TooManyRequests, error.StatusCode);
    }

    [Fact]
    public async Task Network_failures_are_retried_then_wrapped()
    {
        var handler = new StubHandler()
            .Throw(new HttpRequestException("connection refused"))
            .Throw(new HttpRequestException("connection refused"));
        using var client = Clients.With(handler, o => o.MaxRetries = 1);

        var error = await Assert.ThrowsAsync<TesseraNetworkException>(() => client.GetStatsAsync());

        Assert.Equal("GET /v1/stats", error.Operation);
        Assert.IsType<HttpRequestException>(error.InnerException);
        Assert.Equal(2, handler.Requests.Count);
    }

    [Fact]
    public async Task Caller_cancellation_is_not_converted_or_retried()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, "{}");
        using var client = Clients.With(handler);
        using var cts = new CancellationTokenSource();
        cts.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => client.GetStatsAsync(cts.Token));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task Malformed_bodies_raise_serialization_errors()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, "{not json");
        using var client = Clients.With(handler);

        await Assert.ThrowsAsync<TesseraSerializationException>(() => client.GetStatsAsync());
    }

    [Fact]
    public async Task CreateAuditEntry_posts_snake_case_json()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, """
            {"sequence":4,"timestamp":"2026-01-02T03:04:05Z","action":{"kind":"pause"},"actor":"GADMIN","target":null,
             "payload":{},"prev_hash":"aa","entry_hash":"bb"}
            """);
        using var client = Clients.With(handler);

        var entry = await client.CreateAuditEntryAsync(new CreateAuditEntryRequest
        {
            Action = JsonDocument.Parse("""{"kind":"pause"}""").RootElement,
            Actor = "GADMIN",
        });

        Assert.Equal(4ul, entry.Sequence);
        Assert.Equal(new DateTimeOffset(2026, 1, 2, 3, 4, 5, TimeSpan.Zero), entry.Timestamp);
        var (request, body) = handler.Requests.Single();
        Assert.Equal(HttpMethod.Post, request.Method);
        Assert.Equal("""{"action":{"kind":"pause"},"actor":"GADMIN"}""", body);
        Assert.Equal("application/json", request.Content!.Headers.ContentType!.MediaType);
    }

    [Fact]
    public async Task Export_returns_raw_bytes_with_format_and_ledger()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, "address,balance\nGA,10\n");
        using var client = Clients.With(handler);

        var bytes = await client.ExportAssetAsync(3, ExportFormat.Parquet, ledger: 99);

        Assert.Equal("address,balance\nGA,10\n", System.Text.Encoding.UTF8.GetString(bytes));
        Assert.Equal("/v1/assets/3/export?format=parquet&ledger=99", handler.Requests[0].Request.RequestUri!.PathAndQuery);
    }

    [Fact]
    public async Task Portfolio_and_anomaly_queries_use_invariant_formatting()
    {
        var handler = new StubHandler()
            .Respond(HttpStatusCode.OK, """{"address":"GA","summary":{},"holdings":[],"dividends":[],"performance":{"discount_rate":0.05},"time_series":[]}""")
            .Respond(HttpStatusCode.OK, """{"threshold":3.5,"count":0,"flags":[]}""");
        using var client = Clients.With(handler);

        var portfolio = await client.GetHolderPortfolioAsync("GA", new PortfolioQuery
        {
            StartDate = new DateTimeOffset(2026, 1, 1, 0, 0, 0, TimeSpan.Zero),
            DiscountRate = 0.05,
        });
        await client.GetSecurityAnomaliesAsync(minZScore: 3.5);

        Assert.Equal(0.05, portfolio.Performance.DiscountRate);
        Assert.Equal("/v1/holders/GA/portfolio?start_date=2026-01-01T00%3A00%3A00%2B00%3A00&discount_rate=0.05", handler.Requests[0].Request.RequestUri!.PathAndQuery);
        Assert.Equal("/v1/security/anomalies?min_z=3.5", handler.Requests[1].Request.RequestUri!.PathAndQuery);
    }

    [Fact]
    public void Invalid_options_are_rejected()
    {
        Assert.Throws<ArgumentException>(() => TesseraClient.Create(new TesseraClientOptions { BaseAddress = new Uri("ftp://x") }));
        Assert.Throws<ArgumentOutOfRangeException>(() => TesseraClient.Create(new TesseraClientOptions { MaxRetries = -1 }));
    }

    [Fact]
    public void WebSocket_address_is_derived_from_the_base_address()
    {
        Assert.Equal(new Uri("wss://api.tessera.test/v1/ws"), new TesseraClientOptions { BaseAddress = new Uri("https://api.tessera.test/some/path?x=1") }.ResolveWebSocketAddress());
        Assert.Equal(new Uri("ws://localhost:8080/v1/ws"), new TesseraClientOptions().ResolveWebSocketAddress());
        var explicitUri = new Uri("wss://stream.tessera.test/v1/ws");
        Assert.Equal(explicitUri, new TesseraClientOptions { WebSocketAddress = explicitUri }.ResolveWebSocketAddress());
    }

    [Fact]
    public async Task AddTesseraClient_registers_a_factory_backed_typed_client()
    {
        var handler = new StubHandler().Respond(HttpStatusCode.OK, "[]");
        var services = new ServiceCollection();
        services.AddTesseraClient(o =>
            {
                o.BaseAddress = new Uri("https://di.tessera.test");
                o.Timeout = TimeSpan.FromSeconds(12);
            })
            .ConfigurePrimaryHttpMessageHandler(() => handler);
        using var provider = services.BuildServiceProvider();

        var client = provider.GetRequiredService<ITesseraClient>();
        await client.GetAssetDividendsAsync(5);

        Assert.IsType<TesseraClient>(client);
        Assert.Equal("https://di.tessera.test/v1/assets/5/dividends", handler.Requests.Single().Request.RequestUri!.ToString());
        var factory = provider.GetRequiredService<IHttpClientFactory>();
        Assert.Equal(TimeSpan.FromSeconds(12), factory.CreateClient(TesseraServiceCollectionExtensions.HttpClientName).Timeout);
    }

    [Fact]
    public void AddTesseraClient_validates_options_on_resolution()
    {
        var services = new ServiceCollection();
        services.AddTesseraClient(o => o.MaxRetries = -5);
        using var provider = services.BuildServiceProvider();

        Assert.Throws<OptionsValidationException>(() => provider.GetRequiredService<ITesseraClient>());
    }
}
