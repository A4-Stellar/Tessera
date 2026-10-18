package io.tessera.sdk;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.DeserializationFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule;
import io.tessera.sdk.model.AddressCompliance;
import io.tessera.sdk.model.AddressHolding;
import io.tessera.sdk.model.AnchorRecord;
import io.tessera.sdk.model.AnomaliesResponse;
import io.tessera.sdk.model.Asset;
import io.tessera.sdk.model.AssetAnalytics;
import io.tessera.sdk.model.AssetEvents;
import io.tessera.sdk.model.AuditEntriesPage;
import io.tessera.sdk.model.AuditEntry;
import io.tessera.sdk.model.AuditEntryDetail;
import io.tessera.sdk.model.AuditVerificationResult;
import io.tessera.sdk.model.ComplianceSummary;
import io.tessera.sdk.model.CreateAuditEntryRequest;
import io.tessera.sdk.model.Distribution;
import io.tessera.sdk.model.Holder;
import io.tessera.sdk.model.HolderPortfolio;
import io.tessera.sdk.model.PublishAnchorRequest;
import io.tessera.sdk.model.RetryReport;
import io.tessera.sdk.model.Stats;
import io.tessera.sdk.model.TesseraEvent;
import java.io.IOException;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.time.ZonedDateTime;
import java.time.format.DateTimeFormatter;
import java.time.format.DateTimeParseException;
import java.util.Collection;
import java.util.List;
import java.util.Objects;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.TimeUnit;
import java.util.function.Function;

/**
 * Thread-safe client for the Tessera REST API and event stream.
 *
 * <p>Every endpoint has an asynchronous variant returning {@link CompletableFuture}
 * ({@code getStatsAsync()}) and a blocking variant ({@code getStats()}). Blocking calls rethrow the
 * SDK's unchecked exceptions directly; asynchronous calls complete exceptionally with them.
 *
 * <p>Idempotent requests are retried on network failures and 5xx responses; any request is retried
 * on HTTP 429, honouring {@code Retry-After}. Delays grow exponentially with jitter.
 */
public final class TesseraClient {
    static final String USER_AGENT = "tessera-java/0.1.0";

    private static final TypeReference<List<Asset>> ASSETS = new TypeReference<>() { };
    private static final TypeReference<List<Holder>> HOLDERS = new TypeReference<>() { };
    private static final TypeReference<List<Distribution>> DISTRIBUTIONS = new TypeReference<>() { };
    private static final TypeReference<List<AddressHolding>> HOLDINGS = new TypeReference<>() { };
    private static final TypeReference<List<AddressCompliance>> COMPLIANCE = new TypeReference<>() { };
    private static final TypeReference<List<TesseraEvent>> EVENTS = new TypeReference<>() { };
    private static final TypeReference<List<AnchorRecord>> ANCHORS = new TypeReference<>() { };

    private final TesseraClientConfig config;
    private final HttpClient http;
    private final ObjectMapper mapper;

    private TesseraClient(TesseraClientConfig config) {
        this.config = Objects.requireNonNull(config, "config");
        this.http = config.httpClient().orElseGet(() -> HttpClient.newBuilder()
            .connectTimeout(config.timeout())
            .followRedirects(HttpClient.Redirect.NORMAL)
            .build());
        this.mapper = createMapper();
    }

    /** Creates a client for the given configuration. */
    public static TesseraClient create(TesseraClientConfig config) {
        return new TesseraClient(config);
    }

    /** Creates a client with default settings for {@code baseUri}. */
    public static TesseraClient create(URI baseUri) {
        return new TesseraClient(TesseraClientConfig.builder().baseUri(baseUri).build());
    }

    /** The configuration this client was built with. */
    public TesseraClientConfig config() {
        return config;
    }

    /** The Jackson mapper used for requests, responses and event frames. */
    public ObjectMapper objectMapper() {
        return mapper;
    }

    static ObjectMapper createMapper() {
        return JsonMapper.builder()
            .addModule(new JavaTimeModule())
            .disable(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES)
            .disable(DeserializationFeature.ADJUST_DATES_TO_CONTEXT_TIME_ZONE)
            .build();
    }

    // ── Platform ──────────────────────────────────────────────────────────

    /** {@code GET /v1/stats} — platform-wide metrics and TVL. */
    public CompletableFuture<Stats> getStatsAsync() {
        return get("/v1/stats", Stats.class);
    }

    /** Blocking {@link #getStatsAsync()}. */
    public Stats getStats() {
        return await(getStatsAsync());
    }

    /** {@code GET /} — service metadata. */
    public CompletableFuture<JsonNode> getIndexAsync() {
        return get("/", JsonNode.class);
    }

    /** Blocking {@link #getIndexAsync()}. */
    public JsonNode getIndex() {
        return await(getIndexAsync());
    }

    /** {@code GET /version}. */
    public CompletableFuture<JsonNode> getVersionAsync() {
        return get("/version", JsonNode.class);
    }

    /** Blocking {@link #getVersionAsync()}. */
    public JsonNode getVersion() {
        return await(getVersionAsync());
    }

    /** {@code GET /health}. */
    public CompletableFuture<JsonNode> getHealthAsync() {
        return get("/health", JsonNode.class);
    }

    /** Blocking {@link #getHealthAsync()}. */
    public JsonNode getHealth() {
        return await(getHealthAsync());
    }

    /** {@code GET /metrics} — Prometheus exposition text (requires the auth token). */
    public CompletableFuture<String> getMetricsAsync() {
        return send("GET", "/metrics", null).thenApply(r -> new String(r.body(), StandardCharsets.UTF_8));
    }

    /** Blocking {@link #getMetricsAsync()}. */
    public String getMetrics() {
        return await(getMetricsAsync());
    }

    // ── Assets ────────────────────────────────────────────────────────────

    /** {@code GET /v1/assets} — indexed assets, optionally filtered. */
    public CompletableFuture<List<Asset>> listAssetsAsync(ListAssetsQuery query) {
        return get("/v1/assets" + (query == null ? "" : query.toQueryString()), ASSETS);
    }

    /** Blocking {@link #listAssetsAsync(ListAssetsQuery)}. */
    public List<Asset> listAssets(ListAssetsQuery query) {
        return await(listAssetsAsync(query));
    }

    /** {@code GET /v1/assets/{id}}, optionally restricted to a sparse fieldset ({@code fields} may be null). */
    public CompletableFuture<Asset> getAssetAsync(long assetId, String fields) {
        return get("/v1/assets/" + id(assetId) + new QueryString().add("fields", fields), Asset.class);
    }

    /** Blocking {@link #getAssetAsync(long, String)}. */
    public Asset getAsset(long assetId, String fields) {
        return await(getAssetAsync(assetId, fields));
    }

    /** {@code GET /v1/assets/{id}/holders} — holders sorted by balance descending. */
    public CompletableFuture<List<Holder>> getAssetHoldersAsync(long assetId, PageQuery page) {
        return get("/v1/assets/" + id(assetId) + "/holders" + (page == null ? "" : page.toQueryString()), HOLDERS);
    }

    /** Blocking {@link #getAssetHoldersAsync(long, PageQuery)}. */
    public List<Holder> getAssetHolders(long assetId, PageQuery page) {
        return await(getAssetHoldersAsync(assetId, page));
    }

    /** {@code GET /v1/assets/{id}/compliance} — allowlist summary. */
    public CompletableFuture<ComplianceSummary> getAssetComplianceAsync(long assetId) {
        return get("/v1/assets/" + id(assetId) + "/compliance", ComplianceSummary.class);
    }

    /** Blocking {@link #getAssetComplianceAsync(long)}. */
    public ComplianceSummary getAssetCompliance(long assetId) {
        return await(getAssetComplianceAsync(assetId));
    }

    /** {@code GET /v1/assets/{id}/dividends} — every distribution for the asset. */
    public CompletableFuture<List<Distribution>> getAssetDividendsAsync(long assetId) {
        return get("/v1/assets/" + id(assetId) + "/dividends", DISTRIBUTIONS);
    }

    /** Blocking {@link #getAssetDividendsAsync(long)}. */
    public List<Distribution> getAssetDividends(long assetId) {
        return await(getAssetDividendsAsync(assetId));
    }

    /** {@code GET /v1/assets/{id}/distributions/{did}}. */
    public CompletableFuture<Distribution> getAssetDistributionAsync(long assetId, long distributionId) {
        return get("/v1/assets/" + id(assetId) + "/distributions/" + id(distributionId), Distribution.class);
    }

    /** Blocking {@link #getAssetDistributionAsync(long, long)}. */
    public Distribution getAssetDistribution(long assetId, long distributionId) {
        return await(getAssetDistributionAsync(assetId, distributionId));
    }

    /** {@code GET /v1/assets/{id}/analytics} — Gini, HHI and velocity. */
    public CompletableFuture<AssetAnalytics> getAssetAnalyticsAsync(long assetId) {
        return get("/v1/assets/" + id(assetId) + "/analytics", AssetAnalytics.class);
    }

    /** Blocking {@link #getAssetAnalyticsAsync(long)}. */
    public AssetAnalytics getAssetAnalytics(long assetId) {
        return await(getAssetAnalyticsAsync(assetId));
    }

    /** {@code GET /v1/assets/{id}/metrics/analytics} — windowed analytics as raw JSON. */
    public CompletableFuture<JsonNode> getAssetMetricsAnalyticsAsync(long assetId, AnalyticsWindowQuery query) {
        return get("/v1/assets/" + id(assetId) + "/metrics/analytics" + (query == null ? "" : query.toQueryString()), JsonNode.class);
    }

    /** Blocking {@link #getAssetMetricsAnalyticsAsync(long, AnalyticsWindowQuery)}. */
    public JsonNode getAssetMetricsAnalytics(long assetId, AnalyticsWindowQuery query) {
        return await(getAssetMetricsAnalyticsAsync(assetId, query));
    }

    /** {@code GET /v1/assets/{id}/events} — diagnostic events. */
    public CompletableFuture<AssetEvents> getAssetEventsAsync(long assetId, boolean includeDiagnostics) {
        String query = new QueryString().add("include_diagnostics", includeDiagnostics ? Boolean.TRUE : null).toString();
        return get("/v1/assets/" + id(assetId) + "/events" + query, AssetEvents.class);
    }

    /** Blocking {@link #getAssetEventsAsync(long, boolean)}. */
    public AssetEvents getAssetEvents(long assetId, boolean includeDiagnostics) {
        return await(getAssetEventsAsync(assetId, includeDiagnostics));
    }

    /** {@code GET /v1/assets/{id}/export} — cap-table snapshot bytes ({@code ledger} may be null). */
    public CompletableFuture<byte[]> exportAssetAsync(long assetId, ExportFormat format, Long ledger) {
        String query = new QueryString()
            .add("format", (format == null ? ExportFormat.CSV : format).wireName())
            .add("ledger", ledger)
            .toString();
        return send("GET", "/v1/assets/" + id(assetId) + "/export" + query, null).thenApply(HttpResponse::body);
    }

    /** Blocking {@link #exportAssetAsync(long, ExportFormat, Long)}. */
    public byte[] exportAsset(long assetId, ExportFormat format, Long ledger) {
        return await(exportAssetAsync(assetId, format, ledger));
    }

    // ── Holders & compliance ──────────────────────────────────────────────

    /** {@code GET /v1/holders/{address}} — every asset the address holds. */
    public CompletableFuture<List<AddressHolding>> getHolderAsync(String address) {
        return get("/v1/holders/" + segment(address), HOLDINGS);
    }

    /** Blocking {@link #getHolderAsync(String)}. */
    public List<AddressHolding> getHolder(String address) {
        return await(getHolderAsync(address));
    }

    /** {@code GET /v1/holders/{address}/compliance}. */
    public CompletableFuture<List<AddressCompliance>> getHolderComplianceAsync(String address) {
        return get("/v1/holders/" + segment(address) + "/compliance", COMPLIANCE);
    }

    /** Blocking {@link #getHolderComplianceAsync(String)}. */
    public List<AddressCompliance> getHolderCompliance(String address) {
        return await(getHolderComplianceAsync(address));
    }

    /** {@code GET /v1/holders/{address}/portfolio} — TWR, NPV, IRR and time series. */
    public CompletableFuture<HolderPortfolio> getHolderPortfolioAsync(String address, PortfolioQuery query) {
        return get("/v1/holders/" + segment(address) + "/portfolio" + (query == null ? "" : query.toQueryString()), HolderPortfolio.class);
    }

    /** Blocking {@link #getHolderPortfolioAsync(String, PortfolioQuery)}. */
    public HolderPortfolio getHolderPortfolio(String address, PortfolioQuery query) {
        return await(getHolderPortfolioAsync(address, query));
    }

    /** {@code GET /v1/compliance/{address}}. */
    public CompletableFuture<List<AddressCompliance>> getComplianceAsync(String address) {
        return get("/v1/compliance/" + segment(address), COMPLIANCE);
    }

    /** Blocking {@link #getComplianceAsync(String)}. */
    public List<AddressCompliance> getCompliance(String address) {
        return await(getComplianceAsync(address));
    }

    // ── Events & security ─────────────────────────────────────────────────

    /** {@code GET /v1/events} — recent indexed contract events. */
    public CompletableFuture<List<TesseraEvent>> listEventsAsync() {
        return get("/v1/events", EVENTS);
    }

    /** Blocking {@link #listEventsAsync()}. */
    public List<TesseraEvent> listEvents() {
        return await(listEventsAsync());
    }

    /** {@code GET /v1/security/anomalies}; both filters may be null. */
    public CompletableFuture<AnomaliesResponse> getSecurityAnomaliesAsync(String account, Double minZScore) {
        return get("/v1/security/anomalies" + new QueryString().add("account", account).add("min_z", minZScore), AnomaliesResponse.class);
    }

    /** Blocking {@link #getSecurityAnomaliesAsync(String, Double)}. */
    public AnomaliesResponse getSecurityAnomalies(String account, Double minZScore) {
        return await(getSecurityAnomaliesAsync(account, minZScore));
    }

    // ── Audit ─────────────────────────────────────────────────────────────

    /** {@code GET /v1/audit/verify}; {@code entrySequence} may be null. */
    public CompletableFuture<AuditVerificationResult> verifyAuditAsync(Long entrySequence) {
        return get("/v1/audit/verify" + new QueryString().add("entry_seq", entrySequence), AuditVerificationResult.class);
    }

    /** Blocking {@link #verifyAuditAsync(Long)}. */
    public AuditVerificationResult verifyAudit(Long entrySequence) {
        return await(verifyAuditAsync(entrySequence));
    }

    /** {@code GET /v1/audit/entries}. */
    public CompletableFuture<AuditEntriesPage> listAuditEntriesAsync(PageQuery page) {
        return get("/v1/audit/entries" + (page == null ? "" : page.toQueryString()), AuditEntriesPage.class);
    }

    /** Blocking {@link #listAuditEntriesAsync(PageQuery)}. */
    public AuditEntriesPage listAuditEntries(PageQuery page) {
        return await(listAuditEntriesAsync(page));
    }

    /** {@code GET /v1/audit/entries/{sequence}} — entry plus inclusion proof. */
    public CompletableFuture<AuditEntryDetail> getAuditEntryAsync(long sequence) {
        return get("/v1/audit/entries/" + id(sequence), AuditEntryDetail.class);
    }

    /** Blocking {@link #getAuditEntryAsync(long)}. */
    public AuditEntryDetail getAuditEntry(long sequence) {
        return await(getAuditEntryAsync(sequence));
    }

    /** {@code GET /v1/audit/anchors}. */
    public CompletableFuture<List<AnchorRecord>> listAuditAnchorsAsync() {
        return get("/v1/audit/anchors", ANCHORS);
    }

    /** Blocking {@link #listAuditAnchorsAsync()}. */
    public List<AnchorRecord> listAuditAnchors() {
        return await(listAuditAnchorsAsync());
    }

    /** {@code POST /v1/audit/entries}. Not retried on 5xx: the call is not idempotent. */
    public CompletableFuture<AuditEntry> createAuditEntryAsync(CreateAuditEntryRequest request) {
        Objects.requireNonNull(request, "request");
        return post("/v1/audit/entries", request, json -> decode(json, AuditEntry.class, "/v1/audit/entries"));
    }

    /** Blocking {@link #createAuditEntryAsync(CreateAuditEntryRequest)}. */
    public AuditEntry createAuditEntry(CreateAuditEntryRequest request) {
        return await(createAuditEntryAsync(request));
    }

    /** {@code POST /v1/audit/anchor}; {@code request} may be null to anchor at the latest ledger. */
    public CompletableFuture<AnchorRecord> publishAuditAnchorAsync(PublishAnchorRequest request) {
        PublishAnchorRequest body = request == null ? new PublishAnchorRequest(null) : request;
        return post("/v1/audit/anchor", body, json -> decode(json, AnchorRecord.class, "/v1/audit/anchor"));
    }

    /** Blocking {@link #publishAuditAnchorAsync(PublishAnchorRequest)}. */
    public AnchorRecord publishAuditAnchor(PublishAnchorRequest request) {
        return await(publishAuditAnchorAsync(request));
    }

    /** {@code POST /v1/admin/dlq/retry}. */
    public CompletableFuture<RetryReport> retryDeadLetterQueueAsync() {
        return send("POST", "/v1/admin/dlq/retry", null)
            .thenApply(r -> decode(r.body(), RetryReport.class, "/v1/admin/dlq/retry"));
    }

    /** Blocking {@link #retryDeadLetterQueueAsync()}. */
    public RetryReport retryDeadLetterQueue() {
        return await(retryDeadLetterQueueAsync());
    }

    // ── Light-client proofs ───────────────────────────────────────────────

    /** {@code GET /v1/proofs/mmr} — current MMR checkpoint root as raw JSON. */
    public CompletableFuture<JsonNode> getMmrRootAsync() {
        return get("/v1/proofs/mmr", JsonNode.class);
    }

    /** Blocking {@link #getMmrRootAsync()}. */
    public JsonNode getMmrRoot() {
        return await(getMmrRootAsync());
    }

    /** {@code GET /v1/proofs/mmr/{ledger}} — proof for a ledger; {@code account} may be null. */
    public CompletableFuture<JsonNode> getMmrProofAsync(long ledgerSequence, String account) {
        return get("/v1/proofs/mmr/" + id(ledgerSequence) + new QueryString().add("account", account), JsonNode.class);
    }

    /** Blocking {@link #getMmrProofAsync(long, String)}. */
    public JsonNode getMmrProof(long ledgerSequence, String account) {
        return await(getMmrProofAsync(ledgerSequence, account));
    }

    // ── Event stream ──────────────────────────────────────────────────────

    /**
     * Opens the {@code /v1/ws} event stream and subscribes to {@code topics}. A topic matches an
     * event when it occurs, case-insensitively, in the event type, contract ID or payload. Events
     * are delivered to {@code listener} on the HTTP client's executor, in arrival order.
     */
    public CompletableFuture<EventSubscription> subscribeEventsAsync(Collection<String> topics, TesseraEventListener listener) {
        return EventSubscription.open(this, topics, listener);
    }

    /** Blocking {@link #subscribeEventsAsync(Collection, TesseraEventListener)}. */
    public EventSubscription subscribeEvents(Collection<String> topics, TesseraEventListener listener) {
        return await(subscribeEventsAsync(topics, listener));
    }

    HttpClient httpClient() {
        return http;
    }

    // ── Transport ─────────────────────────────────────────────────────────

    /**
     * Waits for {@code future}, rethrowing SDK exceptions unwrapped. Interruption is restored on
     * the calling thread and reported as a {@link TesseraException}.
     */
    static <T> T await(CompletableFuture<T> future) {
        try {
            return future.get();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            future.cancel(true);
            throw new TesseraException("Interrupted while waiting for the Tessera API", e);
        } catch (ExecutionException e) {
            throw rethrow(e.getCause());
        } catch (CancellationException e) {
            throw e;
        }
    }

    private static RuntimeException rethrow(Throwable cause) {
        if (cause instanceof RuntimeException runtime) {
            return runtime;
        }
        if (cause instanceof Error error) {
            throw error;
        }
        return new TesseraException(String.valueOf(cause), cause);
    }

    private static String id(long value) {
        if (value < 0) {
            throw new IllegalArgumentException("identifiers are unsigned, got " + value);
        }
        return Long.toString(value);
    }

    private static String segment(String value) {
        if (value == null || value.isBlank()) {
            throw new IllegalArgumentException("address must not be blank");
        }
        return URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20");
    }

    private <T> CompletableFuture<T> get(String pathAndQuery, Class<T> type) {
        return send("GET", pathAndQuery, null).thenApply(r -> decode(r.body(), type, pathAndQuery));
    }

    private <T> CompletableFuture<T> get(String pathAndQuery, TypeReference<T> type) {
        return send("GET", pathAndQuery, null).thenApply(r -> decode(r.body(), type, pathAndQuery));
    }

    private <T> CompletableFuture<T> post(String path, Object body, Function<byte[], T> decoder) {
        byte[] payload;
        try {
            payload = mapper.writeValueAsBytes(body);
        } catch (JsonProcessingException e) {
            return CompletableFuture.failedFuture(new TesseraSerializationException("Could not encode the request for POST " + path, e));
        }
        return send("POST", path, payload).thenApply(r -> decoder.apply(r.body()));
    }

    private <T> T decode(byte[] body, Class<T> type, String operation) {
        try {
            T value = mapper.readValue(body, type);
            if (value == null) {
                throw new TesseraSerializationException(operation + " returned an empty body", null);
            }
            return value;
        } catch (IOException e) {
            throw new TesseraSerializationException("Could not decode the response of " + operation + " as " + type.getSimpleName(), e);
        }
    }

    private <T> T decode(byte[] body, TypeReference<T> type, String operation) {
        try {
            T value = mapper.readValue(body, type);
            if (value == null) {
                throw new TesseraSerializationException(operation + " returned an empty body", null);
            }
            return value;
        } catch (IOException e) {
            throw new TesseraSerializationException("Could not decode the response of " + operation, e);
        }
    }

    CompletableFuture<HttpResponse<byte[]>> send(String method, String pathAndQuery, byte[] body) {
        HttpRequest.Builder builder = HttpRequest.newBuilder(URI.create(config.baseUri() + pathAndQuery))
            .timeout(config.timeout())
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json");
        config.authToken().ifPresent(token -> builder.header("Authorization", "Bearer " + token));
        config.headers().forEach(builder::setHeader);
        if (body != null) {
            builder.header("Content-Type", "application/json; charset=utf-8");
            builder.method(method, HttpRequest.BodyPublishers.ofByteArray(body));
        } else {
            builder.method(method, HttpRequest.BodyPublishers.noBody());
        }
        boolean idempotent = "GET".equals(method) || "HEAD".equals(method) || "OPTIONS".equals(method);
        return attempt(builder.build(), method + " " + pathAndQuery, idempotent, 0, config.initialBackoff());
    }

    private CompletableFuture<HttpResponse<byte[]>> attempt(
            HttpRequest request, String operation, boolean idempotent, int attempt, Duration backoff) {
        boolean canRetry = attempt < config.maxRetries();
        Duration nextBackoff = min(backoff.multipliedBy(2), config.maxBackoff());
        return http.sendAsync(request, HttpResponse.BodyHandlers.ofByteArray())
            .handle((response, error) -> {
                if (error != null) {
                    Throwable cause = unwrap(error);
                    if (cause instanceof IOException && idempotent && canRetry) {
                        return retryLater(jitter(backoff), () -> attempt(request, operation, idempotent, attempt + 1, nextBackoff));
                    }
                    if (cause instanceof IOException) {
                        return CompletableFuture.<HttpResponse<byte[]>>failedFuture(new TesseraNetworkException(operation, cause));
                    }
                    return CompletableFuture.<HttpResponse<byte[]>>failedFuture(cause);
                }
                int status = response.statusCode();
                if (status >= 200 && status < 300) {
                    return CompletableFuture.completedFuture(response);
                }
                Duration retryAfter = retryAfter(response);
                if (status == 429 && canRetry) {
                    return retryLater(retryAfter != null ? retryAfter : jitter(backoff),
                        () -> attempt(request, operation, idempotent, attempt + 1, nextBackoff));
                }
                if (status >= 500 && idempotent && canRetry) {
                    return retryLater(jitter(backoff), () -> attempt(request, operation, idempotent, attempt + 1, nextBackoff));
                }
                String text = new String(response.body(), StandardCharsets.UTF_8);
                TesseraApiException failure = status == 429
                    ? new TesseraRateLimitException(retryAfter, text)
                    : toApiException(status, text);
                return CompletableFuture.<HttpResponse<byte[]>>failedFuture(failure);
            })
            .thenCompose(Function.identity());
    }

    private static CompletableFuture<HttpResponse<byte[]>> retryLater(
            Duration delay, java.util.function.Supplier<CompletableFuture<HttpResponse<byte[]>>> next) {
        return CompletableFuture.runAsync(() -> { }, CompletableFuture.delayedExecutor(Math.max(0, delay.toMillis()), TimeUnit.MILLISECONDS))
            .thenCompose(ignored -> next.get());
    }

    private TesseraApiException toApiException(int status, String text) {
        try {
            JsonNode node = mapper.readTree(text);
            if (node != null && node.hasNonNull("error") && !node.get("error").asText().isEmpty()) {
                return new TesseraApiException(status, node.get("error").asText(), node.path("message").asText(""));
            }
        } catch (IOException ignored) {
            // Non-JSON error body (e.g. a proxy page): fall through to the raw text.
        }
        return new TesseraApiException(status, "http_" + status, text);
    }

    static Duration retryAfter(HttpResponse<?> response) {
        return response.headers().firstValue("Retry-After").map(TesseraClient::parseRetryAfter).orElse(null);
    }

    static Duration parseRetryAfter(String header) {
        String value = header.trim();
        try {
            long seconds = Long.parseLong(value);
            return seconds > 0 ? Duration.ofSeconds(seconds) : null;
        } catch (NumberFormatException notSeconds) {
            try {
                Duration wait = Duration.between(ZonedDateTime.now(), ZonedDateTime.parse(value, DateTimeFormatter.RFC_1123_DATE_TIME));
                return wait.isNegative() || wait.isZero() ? null : wait;
            } catch (DateTimeParseException notADate) {
                return null;
            }
        }
    }

    private static Throwable unwrap(Throwable error) {
        Throwable current = error;
        while ((current instanceof CompletionException || current instanceof ExecutionException) && current.getCause() != null) {
            current = current.getCause();
        }
        return current;
    }

    private static Duration jitter(Duration delay) {
        long quarter = delay.toNanos() / 4;
        return quarter <= 0 ? delay : delay.plusNanos(ThreadLocalRandom.current().nextLong(quarter));
    }

    private static Duration min(Duration a, Duration b) {
        return a.compareTo(b) <= 0 ? a : b;
    }
}
