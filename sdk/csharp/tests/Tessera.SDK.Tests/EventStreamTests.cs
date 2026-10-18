using System.Collections.Concurrent;
using System.Net;
using System.Net.WebSockets;
using System.Text;
using Xunit;

namespace Tessera.SDK.Tests;

/// <summary>Exercises <see cref="TesseraClient.StreamEventsAsync"/> against a real WebSocket server.</summary>
public sealed class EventStreamTests : IAsyncLifetime
{
    private readonly HttpListener _listener = new();
    private readonly ConcurrentQueue<string> _received = new();
    private readonly TaskCompletionSource<string?> _authorization = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private Func<WebSocket, Task> _script = _ => Task.CompletedTask;
    private Uri _endpoint = null!;
    private Task _serverLoop = Task.CompletedTask;

    public Task InitializeAsync()
    {
        var port = Clients.FreePort();
        _listener.Prefixes.Add($"http://127.0.0.1:{port}/");
        _listener.Start();
        _endpoint = new Uri($"ws://127.0.0.1:{port}/v1/ws");
        _serverLoop = Task.Run(ServeAsync);
        return Task.CompletedTask;
    }

    public async Task DisposeAsync()
    {
        _listener.Stop();
        _listener.Close();
        try
        {
            await _serverLoop;
        }
        catch (Exception ex) when (ex is HttpListenerException or ObjectDisposedException or WebSocketException)
        {
        }
    }

    private async Task ServeAsync()
    {
        var context = await _listener.GetContextAsync();
        _authorization.TrySetResult(context.Request.Headers["Authorization"]);
        var wsContext = await context.AcceptWebSocketAsync(subProtocol: null);
        var socket = wsContext.WebSocket;
        var reader = Task.Run(async () =>
        {
            var buffer = new byte[1024];
            try
            {
                while (socket.State == WebSocketState.Open)
                {
                    var result = await socket.ReceiveAsync(buffer, CancellationToken.None);
                    if (result.MessageType == WebSocketMessageType.Close)
                    {
                        _received.Enqueue("<close>");
                        break;
                    }
                    _received.Enqueue(Encoding.UTF8.GetString(buffer, 0, result.Count));
                }
            }
            catch (WebSocketException)
            {
            }
        });
        await _script(socket);
        await reader;
    }

    private static Task SendAsync(WebSocket socket, string text, bool endOfMessage = true) =>
        socket.SendAsync(Encoding.UTF8.GetBytes(text), WebSocketMessageType.Text, endOfMessage, CancellationToken.None);

    private TesseraClient Client() => TesseraClient.Create(new TesseraClientOptions
    {
        BaseAddress = new Uri("http://unused.test"),
        WebSocketAddress = _endpoint,
        AuthToken = "stream-token",
    });

    private async Task WaitForSubscriptionsAsync(int count)
    {
        for (var i = 0; i < 200 && _received.Count(m => m.StartsWith("subscribe:", StringComparison.Ordinal)) < count; i++)
        {
            await Task.Delay(10);
        }
    }

    [Fact]
    public async Task Streams_subscribed_events_including_fragmented_frames()
    {
        _script = async socket =>
        {
            await WaitForSubscriptionsAsync(2);
            await SendAsync(socket, """{"id":1,"contract":"CASSET","event_type":"transfer","ledger":10,"data":{"amount":"5"}}""");
            await SendAsync(socket, "{\"id\":2,\"contract\":\"CASSET\",", endOfMessage: false);
            await SendAsync(socket, "\"event_type\":\"dividend_claimed\",\"ledger\":11,\"timestamp\":\"2026-01-01T00:00:00Z\",\"data\":null}");
            await socket.CloseOutputAsync(WebSocketCloseStatus.NormalClosure, "done", CancellationToken.None);
        };
        using var client = Client();

        var events = new List<TesseraEvent>();
        await foreach (var evt in client.StreamEventsAsync(new[] { "transfer", " dividend ", "transfer", "" }))
        {
            events.Add(evt);
        }

        Assert.Equal(new[] { "transfer", "dividend_claimed" }, events.Select(e => e.EventType));
        Assert.Equal("5", events[0].Data.GetProperty("amount").GetString());
        Assert.Equal(11u, events[1].Ledger);
        Assert.Equal("Bearer stream-token", await _authorization.Task);
        Assert.Equal(new[] { "subscribe:transfer", "subscribe:dividend" }, _received.Where(m => m.StartsWith("subscribe:", StringComparison.Ordinal)));
    }

    [Fact]
    public async Task Stopping_enumeration_closes_the_connection()
    {
        _script = async socket =>
        {
            await WaitForSubscriptionsAsync(1);
            for (var i = 0; i < 3; i++)
            {
                await SendAsync(socket, "{\"id\":" + i + ",\"contract\":\"C\",\"event_type\":\"transfer\",\"ledger\":1,\"data\":{}}");
            }
        };
        using var client = Client();

        await foreach (var evt in client.StreamEventsAsync(new[] { "transfer" }))
        {
            Assert.Equal(0ul, evt.Id);
            break;
        }

        for (var i = 0; i < 200 && !_received.Contains("<close>"); i++)
        {
            await Task.Delay(10);
        }
        Assert.Contains("<close>", _received);
    }

    [Fact]
    public async Task Cancellation_ends_the_stream()
    {
        _script = async socket =>
        {
            await WaitForSubscriptionsAsync(1);
            await Task.Delay(Timeout.Infinite).WaitAsync(TimeSpan.FromSeconds(5)).ContinueWith(_ => { });
        };
        using var client = Client();
        using var cts = new CancellationTokenSource(TimeSpan.FromMilliseconds(300));

        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () =>
        {
            await foreach (var _ in client.StreamEventsAsync(new[] { "transfer" }, cts.Token))
            {
            }
        });
    }

    [Fact]
    public async Task Malformed_frames_raise_serialization_errors()
    {
        _script = async socket =>
        {
            await WaitForSubscriptionsAsync(1);
            await SendAsync(socket, "not-json");
        };
        using var client = Client();

        await Assert.ThrowsAsync<TesseraSerializationException>(async () =>
        {
            await foreach (var _ in client.StreamEventsAsync(new[] { "transfer" }))
            {
            }
        });
    }

    [Fact]
    public async Task Connection_failures_raise_network_errors()
    {
        using var client = TesseraClient.Create(new TesseraClientOptions
        {
            WebSocketAddress = new Uri($"ws://127.0.0.1:{Clients.FreePort()}/v1/ws"),
        });

        var error = await Assert.ThrowsAsync<TesseraNetworkException>(async () =>
        {
            await foreach (var _ in client.StreamEventsAsync(Array.Empty<string>()))
            {
            }
        });
        Assert.StartsWith("connect ws://127.0.0.1", error.Operation);
    }
}
