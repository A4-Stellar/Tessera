using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Options;

namespace Tessera.SDK;

/// <summary>Registers <see cref="ITesseraClient"/> as an <c>IHttpClientFactory</c> typed client.</summary>
public static class TesseraServiceCollectionExtensions
{
    /// <summary>Name of the underlying named <see cref="HttpClient"/>.</summary>
    public const string HttpClientName = "Tessera.SDK";

    /// <summary>
    /// Adds <see cref="ITesseraClient"/> backed by a pooled <see cref="HttpClient"/> from
    /// <c>IHttpClientFactory</c>. The returned builder can add handlers (Polly, logging, mTLS).
    /// </summary>
    public static IHttpClientBuilder AddTesseraClient(this IServiceCollection services, Action<TesseraClientOptions> configure)
    {
        ArgumentNullException.ThrowIfNull(services);
        ArgumentNullException.ThrowIfNull(configure);
        services.AddOptions<TesseraClientOptions>().Configure(configure).Validate(
            options =>
            {
                try
                {
                    options.Validate();
                    return true;
                }
                catch (ArgumentException)
                {
                    return false;
                }
            },
            "Invalid TesseraClientOptions: BaseAddress must be an absolute http(s) URI, MaxRetries >= 0 and Timeout > 0.");

        return services
            .AddHttpClient<ITesseraClient, TesseraClient>(HttpClientName)
            .ConfigureHttpClient((provider, http) =>
                ConfigureHttpClient(http, provider.GetRequiredService<IOptions<TesseraClientOptions>>().Value));
    }

    internal static void ConfigureHttpClient(HttpClient http, TesseraClientOptions options)
    {
        http.BaseAddress = options.BaseAddress;
        http.Timeout = options.Timeout;
    }
}
