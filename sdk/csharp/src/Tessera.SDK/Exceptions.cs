using System.Net;

namespace Tessera.SDK;

/// <summary>Base type for every error raised by the Tessera SDK.</summary>
public class TesseraException : Exception
{
    /// <inheritdoc />
    public TesseraException(string message, Exception? innerException = null)
        : base(message, innerException)
    {
    }
}

/// <summary>
/// The API answered with a non-success status. <see cref="ErrorCode"/> carries the
/// machine-readable <c>error</c> field of the response body (e.g. <c>not_found</c>).
/// </summary>
public class TesseraApiException : TesseraException
{
    /// <inheritdoc />
    public TesseraApiException(HttpStatusCode statusCode, string errorCode, string apiMessage)
        : base($"Tessera API error ({(int)statusCode} {errorCode}): {apiMessage}")
    {
        StatusCode = statusCode;
        ErrorCode = errorCode;
        ApiMessage = apiMessage;
    }

    /// <summary>HTTP status returned by the API.</summary>
    public HttpStatusCode StatusCode { get; }

    /// <summary>The <c>error</c> code from the response body, or the status text when absent.</summary>
    public string ErrorCode { get; }

    /// <summary>The human-readable <c>message</c> from the response body.</summary>
    public string ApiMessage { get; }
}

/// <summary>The API responded with HTTP 429 and retries were exhausted.</summary>
public sealed class TesseraRateLimitException : TesseraApiException
{
    /// <inheritdoc />
    public TesseraRateLimitException(TimeSpan? retryAfter, string apiMessage)
        : base(HttpStatusCode.TooManyRequests, "rate_limited", apiMessage)
    {
        RetryAfter = retryAfter;
    }

    /// <summary>Delay advertised by the <c>Retry-After</c> header, when present.</summary>
    public TimeSpan? RetryAfter { get; }
}

/// <summary>The request could not reach the API (DNS, connection, TLS, timeout).</summary>
public sealed class TesseraNetworkException : TesseraException
{
    /// <inheritdoc />
    public TesseraNetworkException(string operation, Exception innerException)
        : base($"Tessera network error during {operation}: {innerException.Message}", innerException)
    {
        Operation = operation;
    }

    /// <summary>The operation that failed, e.g. <c>GET /v1/assets</c>.</summary>
    public string Operation { get; }
}

/// <summary>The response body could not be decoded into the expected model.</summary>
public sealed class TesseraSerializationException : TesseraException
{
    /// <inheritdoc />
    public TesseraSerializationException(string message, Exception innerException)
        : base(message, innerException)
    {
    }
}
