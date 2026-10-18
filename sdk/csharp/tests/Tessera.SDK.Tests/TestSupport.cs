using System.Net;
using System.Net.Sockets;
using System.Text;

namespace Tessera.SDK.Tests;

/// <summary>Records requests and replays queued responses.</summary>
internal sealed class StubHandler : HttpMessageHandler
{
    private readonly Queue<Func<HttpRequestMessage, HttpResponseMessage>> _responses = new();

    public List<(HttpRequestMessage Request, string? Body)> Requests { get; } = new();

    public StubHandler Respond(HttpStatusCode status, string body, Action<HttpResponseMessage>? configure = null)
    {
        _responses.Enqueue(_ =>
        {
            var response = new HttpResponseMessage(status) { Content = new StringContent(body, Encoding.UTF8, "application/json") };
            configure?.Invoke(response);
            return response;
        });
        return this;
    }

    public StubHandler Throw(Exception exception)
    {
        _responses.Enqueue(_ => throw exception);
        return this;
    }

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var body = request.Content is null ? null : await request.Content.ReadAsStringAsync(cancellationToken);
        Requests.Add((request, body));
        if (_responses.Count == 0)
        {
            throw new InvalidOperationException($"Unexpected request {request.Method} {request.RequestUri}");
        }
        return _responses.Dequeue()(request);
    }
}

internal static class Clients
{
    public static TesseraClientOptions Options(Action<TesseraClientOptions>? configure = null)
    {
        var options = new TesseraClientOptions
        {
            BaseAddress = new Uri("https://api.tessera.test"),
            InitialBackoff = TimeSpan.FromMilliseconds(1),
            MaxBackoff = TimeSpan.FromMilliseconds(4),
        };
        configure?.Invoke(options);
        return options;
    }

    public static TesseraClient With(StubHandler handler, Action<TesseraClientOptions>? configure = null) =>
        TesseraClient.Create(new HttpClient(handler), Options(configure));

    public static int FreePort()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        return port;
    }
}
