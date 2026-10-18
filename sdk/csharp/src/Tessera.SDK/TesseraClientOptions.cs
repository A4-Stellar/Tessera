namespace Tessera.SDK;

/// <summary>
/// Configuration for <see cref="TesseraClient"/>. Bind it from configuration
/// (<c>services.AddTesseraClient(configuration.GetSection("Tessera"))</c>) or set it in code.
/// </summary>
public sealed class TesseraClientOptions
{
    /// <summary>Base URL of the Tessera REST API, e.g. <c>https://api.tessera.example.com</c>.</summary>
    public Uri BaseAddress { get; set; } = new("http://localhost:8080");

    /// <summary>
    /// WebSocket endpoint for the event stream. When unset it is derived from
    /// <see cref="BaseAddress"/> (<c>http</c> → <c>ws</c>, <c>https</c> → <c>wss</c>) with the path <c>/v1/ws</c>.
    /// </summary>
    public Uri? WebSocketAddress { get; set; }

    /// <summary>Per-request timeout applied to the underlying <see cref="HttpClient"/>.</summary>
    public TimeSpan Timeout { get; set; } = TimeSpan.FromSeconds(30);

    /// <summary>Maximum retries for idempotent requests and rate-limited responses.</summary>
    public int MaxRetries { get; set; } = 3;

    /// <summary>First retry delay; doubled on each attempt up to <see cref="MaxBackoff"/>.</summary>
    public TimeSpan InitialBackoff { get; set; } = TimeSpan.FromMilliseconds(200);

    /// <summary>Upper bound for the retry delay.</summary>
    public TimeSpan MaxBackoff { get; set; } = TimeSpan.FromSeconds(3);

    /// <summary>Bearer token sent as <c>Authorization: Bearer …</c> on REST and WebSocket requests.</summary>
    public string? AuthToken { get; set; }

    /// <summary>Extra headers added to every outbound request.</summary>
    public IDictionary<string, string> CustomHeaders { get; } = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

    internal Uri ResolveWebSocketAddress()
    {
        if (WebSocketAddress is not null)
        {
            return WebSocketAddress;
        }

        var builder = new UriBuilder(BaseAddress)
        {
            Scheme = BaseAddress.Scheme == Uri.UriSchemeHttps ? "wss" : "ws",
            Path = "/v1/ws",
            Query = string.Empty,
        };
        return builder.Uri;
    }

    internal void Validate()
    {
        if (!BaseAddress.IsAbsoluteUri || (BaseAddress.Scheme != Uri.UriSchemeHttp && BaseAddress.Scheme != Uri.UriSchemeHttps))
        {
            throw new ArgumentException($"BaseAddress must be an absolute http(s) URI, got '{BaseAddress}'.", nameof(BaseAddress));
        }
        if (MaxRetries < 0)
        {
            throw new ArgumentOutOfRangeException(nameof(MaxRetries), MaxRetries, "MaxRetries cannot be negative.");
        }
        if (Timeout <= TimeSpan.Zero && Timeout != System.Threading.Timeout.InfiniteTimeSpan)
        {
            throw new ArgumentOutOfRangeException(nameof(Timeout), Timeout, "Timeout must be positive.");
        }
    }
}
