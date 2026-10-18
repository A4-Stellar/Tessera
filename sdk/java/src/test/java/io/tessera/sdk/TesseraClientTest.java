package io.tessera.sdk;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import io.tessera.sdk.model.Asset;
import io.tessera.sdk.model.AssetAnalytics;
import io.tessera.sdk.model.AuditEntry;
import io.tessera.sdk.model.CreateAuditEntryRequest;
import io.tessera.sdk.model.Stats;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class TesseraClientTest {
    private StubServer server;

    @BeforeEach
    void start() throws Exception {
        server = new StubServer();
    }

    @AfterEach
    void stop() {
        server.close();
    }

    private TesseraClient client(int maxRetries) {
        return TesseraClient.create(TesseraClientConfig.builder()
            .baseUri(server.uri())
            .maxRetries(maxRetries)
            .backoff(Duration.ofMillis(1), Duration.ofMillis(4))
            .authToken("secret")
            .header("X-Tenant", "acme")
            .build());
    }

    @Test
    void getStatsDecodesSnakeCaseAndSendsHeaders() {
        server.reply(200, """
            {"total_assets":3,"active_assets":2,"tvl_cents":"123456789012345678901","tvl_usd":1234567.89,
             "total_holders":40,"total_distributions":5,"last_indexed_ledger":51234,"last_updated":null,"future_field":1}
            """);

        Stats stats = client(0).getStats();

        assertEquals(3, stats.totalAssets());
        assertEquals("123456789012345678901", stats.tvlCents());
        assertEquals(51234L, stats.lastIndexedLedger());
        assertNull(stats.lastUpdated());
        StubServer.Recorded request = server.requests.get(0);
        assertEquals("GET", request.method());
        assertEquals("/v1/stats", request.pathAndQuery());
        assertEquals("Bearer secret", request.header("Authorization"));
        assertEquals("acme", request.header("X-Tenant"));
        assertTrue(request.header("User-Agent").startsWith("tessera-java/"));
        assertEquals("application/json", request.header("Accept"));
    }

    @Test
    void asyncVariantCompletesWithTheSameModel() {
        server.reply(200, """
            [{"id":7,"token_contract":"CTOKEN","name":"Loft","symbol":"LOFT","asset_type":"real_estate","active":true,"holders":12}]
            """);

        List<Asset> assets = client(0).listAssetsAsync(new ListAssetsQuery("real_estate", true, null, 10, "id,name")).join();

        assertEquals(1, assets.size());
        assertEquals(7L, assets.get(0).id());
        assertTrue(assets.get(0).active());
        assertEquals("/v1/assets?asset_type=real_estate&active=true&limit=10&fields=id%2Cname", server.requests.get(0).pathAndQuery());
    }

    @Test
    void analyticsMapsTwentyFourHourFieldsAndOptionalMetrics() {
        server.reply(200, """
            {"asset_id":1,"holder_count":4,"gini_coefficient":0.42,"herfindahl_hirschman_index":null,
             "volume_24h":"900","total_supply":"1000","velocity_24h":0.9,"window_start":"a","window_end":"b"}
            """);

        AssetAnalytics analytics = client(0).getAssetAnalytics(1);

        assertEquals(0.42, analytics.giniCoefficient());
        assertNull(analytics.herfindahlHirschmanIndex());
        assertEquals("900", analytics.volume24h());
        assertEquals(0.9, analytics.velocity24h());
    }

    @Test
    void addressSegmentsAreEscaped() {
        server.reply(200, "[]");

        client(0).getHolderCompliance("G ABC/../x");

        assertEquals("/v1/holders/G%20ABC%2F..%2Fx/compliance", server.requests.get(0).pathAndQuery());
    }

    @Test
    void apiErrorsCarryCodeAndMessage() {
        server.reply(404, "{\"error\":\"not_found\",\"message\":\"no asset with id 9\"}");

        TesseraApiException error = assertThrows(TesseraApiException.class, () -> client(3).getAsset(9, null));

        assertEquals(404, error.statusCode());
        assertEquals("not_found", error.errorCode());
        assertEquals("no asset with id 9", error.apiMessage());
        assertEquals(1, server.requests.size());
    }

    @Test
    void asyncFailuresCompleteExceptionallyWithSdkExceptions() {
        server.reply(400, "{\"error\":\"bad_request\",\"message\":\"bad window\"}");

        CompletionException wrapped = assertThrows(CompletionException.class,
            () -> client(0).getAssetMetricsAnalyticsAsync(1, new AnalyticsWindowQuery("1h", "sliding", 5)).join());

        TesseraApiException error = assertInstanceOf(TesseraApiException.class, wrapped.getCause());
        assertEquals("bad_request", error.errorCode());
        assertEquals("/v1/assets/1/metrics/analytics?window=1h&window_type=sliding&limit=5", server.requests.get(0).pathAndQuery());
    }

    @Test
    void nonJsonErrorBodiesAreKeptVerbatim() {
        server.reply(502, "<html>bad gateway</html>");

        TesseraApiException error = assertThrows(TesseraApiException.class, () -> client(0).getStats());

        assertEquals("http_502", error.errorCode());
        assertEquals("<html>bad gateway</html>", error.apiMessage());
    }

    @Test
    void idempotentRequestsRetryServerErrors() {
        server.reply(503, "{\"error\":\"service_unavailable\",\"message\":\"warming up\"}")
            .reply(500, "oops")
            .reply(200, "[]");

        assertTrue(client(3).listEvents().isEmpty());
        assertEquals(3, server.requests.size());
    }

    @Test
    void postsAreNotRetriedOnServerErrors() {
        server.reply(500, "{\"error\":\"internal\",\"message\":\"boom\"}");

        assertThrows(TesseraApiException.class, () -> client(3).retryDeadLetterQueue());
        assertEquals(1, server.requests.size());
    }

    @Test
    void rateLimitsRetryThenRaiseWithRetryAfter() {
        server.reply(429, "slow down", Map.of("Retry-After", "1"))
            .reply(429, "slow down", Map.of("Retry-After", "1"));

        TesseraRateLimitException error = assertThrows(TesseraRateLimitException.class, () -> client(1).getStats());

        assertEquals(2, server.requests.size());
        assertEquals(Duration.ofSeconds(1), error.retryAfter().orElseThrow());
        assertEquals(429, error.statusCode());
    }

    @Test
    void networkFailuresAreRetriedThenWrapped() {
        TesseraClient client = TesseraClient.create(TesseraClientConfig.builder()
            .baseUri(URI.create("http://127.0.0.1:1"))
            .maxRetries(1)
            .backoff(Duration.ofMillis(1), Duration.ofMillis(2))
            .timeout(Duration.ofSeconds(2))
            .build());

        TesseraNetworkException error = assertThrows(TesseraNetworkException.class, client::getStats);

        assertEquals("GET /v1/stats", error.operation());
    }

    @Test
    void malformedBodiesRaiseSerializationErrors() {
        server.reply(200, "{not json");

        assertThrows(TesseraSerializationException.class, () -> client(0).getStats());
    }

    @Test
    void createAuditEntryPostsSnakeCaseJsonWithoutNulls() throws Exception {
        server.reply(200, """
            {"sequence":4,"timestamp":"2026-01-02T03:04:05Z","action":{"kind":"pause"},"actor":"GADMIN","target":null,
             "payload":{},"prev_hash":"aa","entry_hash":"bb"}
            """);
        ObjectMapper mapper = new ObjectMapper();

        AuditEntry entry = client(3).createAuditEntry(
            new CreateAuditEntryRequest(mapper.readTree("{\"kind\":\"pause\"}"), "GADMIN", null, null));

        assertEquals(4L, entry.sequence());
        assertEquals(OffsetDateTime.of(2026, 1, 2, 3, 4, 5, 0, ZoneOffset.UTC), entry.timestamp());
        StubServer.Recorded request = server.requests.get(0);
        assertEquals("POST", request.method());
        assertEquals("{\"action\":{\"kind\":\"pause\"},\"actor\":\"GADMIN\"}", request.body());
        assertTrue(request.header("Content-Type").startsWith("application/json"));
    }

    @Test
    void exportReturnsRawBytesWithFormatAndLedger() {
        server.reply(200, "address,balance\nGA,10\n");

        byte[] bytes = client(0).exportAsset(3, ExportFormat.PARQUET, 99L);

        assertArrayEquals("address,balance\nGA,10\n".getBytes(StandardCharsets.UTF_8), bytes);
        assertEquals("/v1/assets/3/export?format=parquet&ledger=99", server.requests.get(0).pathAndQuery());
    }

    @Test
    void portfolioQueryUsesIsoTimestamps() {
        server.reply(200, "{\"address\":\"GA\",\"summary\":{},\"performance\":{\"discount_rate\":0.05}}");

        var portfolio = client(0).getHolderPortfolio("GA",
            new PortfolioQuery(OffsetDateTime.of(2026, 1, 1, 0, 0, 0, 0, ZoneOffset.UTC), null, 0.05));

        assertEquals(0.05, portfolio.performance().discountRate());
        assertTrue(portfolio.holdings().isEmpty());
        assertEquals("/v1/holders/GA/portfolio?start_date=2026-01-01T00%3A00%3A00Z&discount_rate=0.05", server.requests.get(0).pathAndQuery());
    }

    @Test
    void configurationIsValidated() {
        assertThrows(IllegalArgumentException.class, () -> TesseraClientConfig.builder().baseUri(URI.create("ftp://x")).build());
        assertThrows(IllegalArgumentException.class, () -> TesseraClientConfig.builder().maxRetries(-1).build());
        assertThrows(IllegalArgumentException.class, () -> TesseraClientConfig.builder().timeout(Duration.ZERO).build());
        assertThrows(IllegalArgumentException.class, () -> client(0).getAsset(-1, null));
    }

    @Test
    void webSocketUriIsDerivedFromTheBaseUri() {
        assertEquals(URI.create("wss://api.tessera.test/v1/ws"),
            TesseraClientConfig.builder().baseUri(URI.create("https://api.tessera.test/")).build().webSocketUri());
        assertEquals(URI.create("ws://localhost:8080/v1/ws"), TesseraClientConfig.builder().build().webSocketUri());
        URI explicit = URI.create("wss://stream.tessera.test/v1/ws");
        assertEquals(explicit, TesseraClientConfig.builder().webSocketUri(explicit).build().webSocketUri());
    }

    @Test
    void retryAfterParsesSecondsAndDates() {
        assertEquals(Duration.ofSeconds(7), TesseraClient.parseRetryAfter("7"));
        assertNull(TesseraClient.parseRetryAfter("0"));
        assertNull(TesseraClient.parseRetryAfter("soon"));
        assertNull(TesseraClient.parseRetryAfter("Wed, 21 Oct 2015 07:28:00 GMT"));
    }
}
