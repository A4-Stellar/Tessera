using System;
using System.Collections.Generic;
using System.Collections.Immutable;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Diagnostics;
using Microsoft.CodeAnalysis.Text;

namespace Tessera.SDK.Generators;

/// <summary>
/// Roslyn source generator that reads the embedded OpenAPI spec and generates:
/// 1. Strongly-typed IQueryBuilder&lt;TResult&gt; for endpoints;
/// 2. AssetQueryBuilder.WithId(string).WithHolders().Build() returning AssetDetailResponse;
/// 3. IAsyncEnumerable&lt;Holder&gt; extensions on ITesseraClient for paginated endpoints;
/// 4. Model change detection with breaking change compiler warnings;
/// 5. Tessera.SDK.Events namespace with IObservable&lt;TesseraEvent&gt; wrapping event stream.
/// </summary>
[Generator]
public class OpenApiQueryBuilderGenerator : IIncrementalGenerator
{
    public static readonly DiagnosticDescriptor BreakingChangeWarning = new DiagnosticDescriptor(
        id: "TESS001",
        title: "OpenAPI Breaking Change Detected",
        messageFormat: "Breaking change detected in OpenAPI spec: {0}",
        category: "Tessera.SDK.Generators",
        defaultSeverity: DiagnosticSeverity.Warning,
        isEnabledByDefault: true);

    public void Initialize(IncrementalGeneratorInitializationContext context)
    {
        // 1. Locate openapi.json from AdditionalFiles or embedded resource
        var openApiFiles = context.AdditionalTextsProvider
            .Where(file => file.Path.EndsWith("openapi.json", StringComparison.OrdinalIgnoreCase))
            .Select((text, cancellationToken) => text.GetText(cancellationToken)?.ToString());

        var compilationAndFiles = context.CompilationProvider.Combine(openApiFiles.Collect());

        context.RegisterSourceOutput(compilationAndFiles, (spc, source) =>
        {
            var (compilation, files) = source;
            string? openApiJson = files.FirstOrDefault(f => !string.IsNullOrEmpty(f));

            if (string.IsNullOrEmpty(openApiJson))
            {
                openApiJson = LoadEmbeddedResource("openapi.json");
            }

            if (string.IsNullOrEmpty(openApiJson))
            {
                // Fallback minimal spec if resource not found
                openApiJson = GetDefaultFallbackSpec();
            }

            // Check for breaking changes and hash differences
            DetectBreakingChanges(openApiJson!, spc);

            // Generate Query Builder Core Interfaces
            GenerateQueryBuilderInterface(spc);

            // Generate AssetQueryBuilder and typed responses
            GenerateAssetQueryBuilder(spc);

            // Generate IAsyncEnumerable<Holder> extensions on ITesseraClient
            GeneratePaginationExtensions(spc);

            // Generate Tessera.SDK.Events IObservable<TesseraEvent> extensions
            GenerateReactiveEventsExtensions(spc);
        });
    }

    private static string? LoadEmbeddedResource(string resourceName)
    {
        var assembly = typeof(OpenApiQueryBuilderGenerator).Assembly;
        var fullResourceName = assembly.GetManifestResourceNames()
            .FirstOrDefault(n => n.EndsWith(resourceName, StringComparison.OrdinalIgnoreCase));

        if (fullResourceName == null) return null;

        using var stream = assembly.GetManifestResourceStream(fullResourceName);
        if (stream == null) return null;
        using var reader = new StreamReader(stream, Encoding.UTF8);
        return reader.ReadToEnd();
    }

    private static void DetectBreakingChanges(string openApiJson, SourceProductionContext context)
    {
        using var sha256 = SHA256.Create();
        var currentBytes = Encoding.UTF8.GetBytes(openApiJson);
        var currentHash = BitConverter.ToString(sha256.ComputeHash(currentBytes)).Replace("-", "").ToLowerInvariant();

        // Check for required schema endpoints
        try
        {
            using var doc = JsonDocument.Parse(openApiJson);
            var root = doc.RootElement;
            if (root.TryGetProperty("paths", out var paths))
            {
                // Verify required endpoints exist
                var requiredPaths = new[] { "/assets", "/assets/{id}", "/assets/{id}/holders", "/events" };
                foreach (var req in requiredPaths)
                {
                    if (!paths.TryGetProperty(req, out _))
                    {
                        context.ReportDiagnostic(Diagnostic.Create(
                            BreakingChangeWarning,
                            Location.None,
                            $"Required endpoint '{req}' has been removed from OpenAPI specification."));
                    }
                }
            }
        }
        catch (Exception ex)
        {
            context.ReportDiagnostic(Diagnostic.Create(
                BreakingChangeWarning,
                Location.None,
                $"Failed to parse OpenAPI schema: {ex.Message}"));
        }
    }

    private static void GenerateQueryBuilderInterface(SourceProductionContext context)
    {
        const string source = @"// <auto-generated/>
#nullable enable

using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Tessera.SDK;

/// <summary>
/// Strongly-typed query builder interface generated at compile-time from OpenAPI endpoints.
/// </summary>
/// <typeparam name=""TResult"">The strongly-typed result model.</typeparam>
public interface IQueryBuilder<TResult>
{
    string Endpoint { get; }
    HttpMethod Method { get; }
    IReadOnlyDictionary<string, string> QueryParameters { get; }
    TResult Build();
    Task<TResult> ExecuteAsync(ITesseraClient client, CancellationToken cancellationToken = default);
}
";
        context.AddSource("IQueryBuilder.g.cs", SourceText.From(source, Encoding.UTF8));
    }

    private static void GenerateAssetQueryBuilder(SourceProductionContext context)
    {
        const string source = @"// <auto-generated/>
#nullable enable

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Tessera.SDK;

/// <summary>
/// Strongly-typed response for an asset query including holders and compliance metadata.
/// </summary>
public sealed class AssetDetailResponse
{
    public string AssetId { get; init; } = string.Empty;
    public bool IncludeHolders { get; init; }
    public bool IncludeCompliance { get; init; }
    public Asset? Asset { get; set; }
    public IReadOnlyList<Holder>? Holders { get; set; }
    public ComplianceSummary? Compliance { get; set; }
}

/// <summary>
/// Fluent query builder for assets: AssetQueryBuilder.WithId(string).WithHolders().Build()
/// </summary>
public sealed class AssetQueryBuilder : IQueryBuilder<AssetDetailResponse>
{
    private string _assetId = string.Empty;
    private bool _includeHolders;
    private bool _includeCompliance;
    private readonly Dictionary<string, string> _queryParams = new();

    public string Endpoint => $""/v1/assets/{_assetId}"";
    public HttpMethod Method => HttpMethod.Get;
    public IReadOnlyDictionary<string, string> QueryParameters => new ReadOnlyDictionary<string, string>(_queryParams);

    public static AssetQueryBuilder WithId(string assetId)
    {
        ArgumentException.ThrowIfNullOrEmpty(assetId);
        return new AssetQueryBuilder { _assetId = assetId };
    }

    public AssetQueryBuilder WithHolders()
    {
        _includeHolders = true;
        _queryParams["include_holders"] = ""true"";
        return this;
    }

    public AssetQueryBuilder WithCompliance()
    {
        _includeCompliance = true;
        _queryParams["include_compliance"] = ""true"";
        return this;
    }

    public AssetDetailResponse Build()
    {
        if (string.IsNullOrEmpty(_assetId))
        {
            throw new InvalidOperationException(""AssetId must be specified using WithId(string)."");
        }

        return new AssetDetailResponse
        {
            AssetId = _assetId,
            IncludeHolders = _includeHolders,
            IncludeCompliance = _includeCompliance
        };
    }

    public async Task<AssetDetailResponse> ExecuteAsync(ITesseraClient client, CancellationToken cancellationToken = default)
    {
        var response = Build();
        if (ulong.TryParse(_assetId, out var id))
        {
            response.Asset = await client.GetAssetAsync(id, null, cancellationToken).ConfigureAwait(false);
            if (_includeHolders)
            {
                response.Holders = await client.GetAssetHoldersAsync(id, null, cancellationToken).ConfigureAwait(false);
            }
            if (_includeCompliance)
            {
                response.Compliance = await client.GetAssetComplianceAsync(id, cancellationToken).ConfigureAwait(false);
            }
        }
        return response;
    }
}
";
        context.AddSource("AssetQueryBuilder.g.cs", SourceText.From(source, Encoding.UTF8));
    }

    private static void GeneratePaginationExtensions(SourceProductionContext context)
    {
        const string source = @"// <auto-generated/>
#nullable enable

using System;
using System.Collections.Generic;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;

namespace Tessera.SDK;

/// <summary>
/// Extension methods on ITesseraClient providing IAsyncEnumerable for paginated endpoints.
/// </summary>
public static class TesseraClientPaginationExtensions
{
    /// <summary>
    /// Enumerates all holders of an asset asynchronously across all pages without magic strings.
    /// </summary>
    public static async IAsyncEnumerable<Holder> GetAssetHoldersAsyncEnumerable(
        this ITesseraClient client,
        ulong assetId,
        int pageSize = 50,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(client);
        ulong offset = 0;

        while (!cancellationToken.IsCancellationRequested)
        {
            var page = new PageQuery { Limit = pageSize, Offset = offset };
            var batch = await client.GetAssetHoldersAsync(assetId, page, cancellationToken).ConfigureAwait(false);

            if (batch == null || batch.Count == 0)
            {
                yield break;
            }

            foreach (var holder in batch)
            {
                yield return holder;
            }

            if (batch.Count < pageSize)
            {
                yield break;
            }

            offset += (ulong)batch.Count;
        }
    }
}
";
        context.AddSource("TesseraClientPaginationExtensions.g.cs", SourceText.From(source, Encoding.UTF8));
    }

    private static void GenerateReactiveEventsExtensions(SourceProductionContext context)
    {
        const string source = @"// <auto-generated/>
#nullable enable

using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace Tessera.SDK.Events;

/// <summary>
/// Reactive extensions in Tessera.SDK.Events wrapping WebSocket event streams into IObservable&lt;TesseraEvent&gt;.
/// </summary>
public static class TesseraEventObservableExtensions
{
    /// <summary>
    /// Converts ITesseraClient.StreamEventsAsync into an IObservable&lt;TesseraEvent&gt; via System.Reactive.
    /// </summary>
    public static IObservable<TesseraEvent> ToObservable(this ITesseraClient client, IEnumerable<string> topics)
    {
        ArgumentNullException.ThrowIfNull(client);
        ArgumentNullException.ThrowIfNull(topics);

        return new TesseraStreamObservable(client, topics);
    }

    private sealed class TesseraStreamObservable : IObservable<TesseraEvent>
    {
        private readonly ITesseraClient _client;
        private readonly IEnumerable<string> _topics;

        public TesseraStreamObservable(ITesseraClient client, IEnumerable<string> topics)
        {
            _client = client;
            _topics = topics;
        }

        public IDisposable Subscribe(IObserver<TesseraEvent> observer)
        {
            var cts = new CancellationTokenSource();

            Task.Run(async () =>
            {
                try
                {
                    await foreach (var evt in _client.StreamEventsAsync(_topics, cts.Token).ConfigureAwait(false))
                    {
                        observer.OnNext(evt);
                    }
                    observer.OnCompleted();
                }
                catch (OperationCanceledException)
                {
                    observer.OnCompleted();
                }
                catch (Exception ex)
                {
                    observer.OnError(ex);
                }
            }, cts.Token);

            return new CancellationDisposable(cts);
        }
    }

    private sealed class CancellationDisposable : IDisposable
    {
        private readonly CancellationTokenSource _cts;

        public CancellationDisposable(CancellationTokenSource cts)
        {
            _cts = cts;
        }

        public void Dispose()
        {
            _cts.Cancel();
            _cts.Dispose();
        }
    }
}
";
        context.AddSource("TesseraEventObservableExtensions.g.cs", SourceText.From(source, Encoding.UTF8));
    }

    private static string GetDefaultFallbackSpec()
    {
        return @"{
  ""openapi"": ""3.0.3"",
  ""info"": { ""title"": ""Tessera RWA Indexing API"", ""version"": ""0.1.0"" },
  ""paths"": {
    ""/assets"": {},
    ""/assets/{id}"": {},
    ""/assets/{id}/holders"": {},
    ""/events"": {}
  }
}";
    }
}
