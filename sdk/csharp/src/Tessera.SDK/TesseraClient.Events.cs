using System.Buffers;
using System.Net.WebSockets;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

namespace Tessera.SDK;

public sealed partial class TesseraClient
{
    private const int ReceiveBufferSize = 8 * 1024;

    /// <summary>Upper bound for one event frame; larger messages are treated as a protocol error.</summary>
    internal const int MaxMessageBytes = 4 * 1024 * 1024;

    /// <summary>Factory for the socket, replaceable in tests.</summary>
    internal Func<ClientWebSocket> WebSocketFactory { get; set; } = static () => new ClientWebSocket();

    /// <inheritdoc />
    public async IAsyncEnumerable<TesseraEvent> StreamEventsAsync(
        IEnumerable<string> topics,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(topics);
        var subscriptions = topics.Select(t => t?.Trim()).Where(t => !string.IsNullOrEmpty(t)).Distinct(StringComparer.Ordinal).ToArray();
        var endpoint = _options.ResolveWebSocketAddress();

        using var socket = WebSocketFactory();
        socket.Options.SetRequestHeader("User-Agent", UserAgent);
        if (!string.IsNullOrEmpty(_options.AuthToken))
        {
            socket.Options.SetRequestHeader("Authorization", $"Bearer {_options.AuthToken}");
        }
        foreach (var (name, value) in _options.CustomHeaders)
        {
            socket.Options.SetRequestHeader(name, value);
        }

        try
        {
            await socket.ConnectAsync(endpoint, cancellationToken).ConfigureAwait(false);
            foreach (var topic in subscriptions)
            {
                // Server protocol (api/src/ws.rs): one text frame per topic, "subscribe:<topic>".
                await socket.SendAsync(Encoding.UTF8.GetBytes($"subscribe:{topic}"), WebSocketMessageType.Text, endOfMessage: true, cancellationToken).ConfigureAwait(false);
            }
        }
        catch (Exception ex) when (ex is WebSocketException or HttpRequestException)
        {
            throw new TesseraNetworkException($"connect {endpoint}", ex);
        }

        var buffer = ArrayPool<byte>.Shared.Rent(ReceiveBufferSize);
        var message = new ArrayBufferWriter<byte>(ReceiveBufferSize);
        try
        {
            while (true)
            {
                message.Clear();
                WebSocketReceiveResult result;
                try
                {
                    do
                    {
                        result = await socket.ReceiveAsync(buffer.AsMemory(0, ReceiveBufferSize).ToArraySegment(), cancellationToken).ConfigureAwait(false);
                        if (result.MessageType == WebSocketMessageType.Close)
                        {
                            yield break;
                        }
                        if (message.WrittenCount + result.Count > MaxMessageBytes)
                        {
                            throw new TesseraException($"Event frame exceeded {MaxMessageBytes} bytes.");
                        }
                        message.Write(buffer.AsSpan(0, result.Count));
                    }
                    while (!result.EndOfMessage);
                }
                catch (WebSocketException ex) when (ex.WebSocketErrorCode == WebSocketError.ConnectionClosedPrematurely)
                {
                    throw new TesseraNetworkException($"receive {endpoint}", ex);
                }

                if (result.MessageType != WebSocketMessageType.Text || message.WrittenCount == 0)
                {
                    continue;
                }

                TesseraEvent? evt;
                try
                {
                    evt = JsonSerializer.Deserialize<TesseraEvent>(message.WrittenSpan, TesseraJson.Options);
                }
                catch (JsonException ex)
                {
                    throw new TesseraSerializationException("Could not decode an event frame from the Tessera event stream.", ex);
                }

                if (evt is not null)
                {
                    yield return evt;
                }
            }
        }
        finally
        {
            ArrayPool<byte>.Shared.Return(buffer);
            await CloseQuietlyAsync(socket).ConfigureAwait(false);
        }
    }

    private static async Task CloseQuietlyAsync(ClientWebSocket socket)
    {
        if (socket.State is not (WebSocketState.Open or WebSocketState.CloseReceived))
        {
            return;
        }
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(2));
        try
        {
            await socket.CloseOutputAsync(WebSocketCloseStatus.NormalClosure, "client closing", timeout.Token).ConfigureAwait(false);
        }
        catch (Exception ex) when (ex is WebSocketException or OperationCanceledException)
        {
            // Best effort: the server may already have dropped the connection.
        }
    }
}

internal static class MemoryExtensions
{
    public static ArraySegment<byte> ToArraySegment(this Memory<byte> memory) =>
        System.Runtime.InteropServices.MemoryMarshal.TryGetArray<byte>(memory, out var segment)
            ? segment
            : throw new InvalidOperationException("Receive buffer must be array-backed.");
}
