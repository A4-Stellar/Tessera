using System;
using System.Collections.Immutable;
using System.IO;
using System.Linq;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Text;
using Xunit;

namespace Tessera.SDK.Generators.Tests;

public class OpenApiGeneratorTests
{
    private static (ImmutableArray<Diagnostic> Diagnostics, ImmutableArray<GeneratedSourceResult> GeneratedSources) RunGenerator(
        string userSource,
        string? openApiJson = null)
    {
        var syntaxTree = CSharpSyntaxTree.ParseText(userSource);
        var references = AppDomain.CurrentDomain.GetAssemblies()
            .Where(a => !a.IsDynamic && !string.IsNullOrWhiteSpace(a.Location))
            .Select(a => MetadataReference.CreateFromFile(a.Location))
            .Cast<MetadataReference>();

        var compilation = CSharpCompilation.Create(
            "GeneratorTestAssembly",
            new[] { syntaxTree },
            references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary));

        var generator = new OpenApiQueryBuilderGenerator();

        GeneratorDriver driver = CSharpGeneratorDriver.Create(generator);
        if (openApiJson != null)
        {
            var additionalText = new InMemoryAdditionalText("openapi.json", openApiJson);
            driver = driver.AddAdditionalTexts(ImmutableArray.Create<AdditionalText>(additionalText));
        }

        driver = driver.RunGeneratorsAndUpdateCompilation(compilation, out var outputCompilation, out var diagnostics);

        var runResult = driver.GetRunResult();
        var generatedSources = runResult.Results
            .SelectMany(r => r.GeneratedSources)
            .ToImmutableArray();

        return (diagnostics, generatedSources);
    }

    [Fact]
    public void Generator_Generates_IQueryBuilder_Interface()
    {
        var (diagnostics, sources) = RunGenerator("public class Dummy {}");

        var queryBuilderSource = sources.FirstOrDefault(s => s.HintName == "IQueryBuilder.g.cs");
        Assert.NotNull(queryBuilderSource.SourceText);

        var code = queryBuilderSource.SourceText.ToString();
        Assert.Contains("public interface IQueryBuilder<TResult>", code);
        Assert.Contains("string Endpoint { get; }", code);
        Assert.Contains("TResult Build();", code);
    }

    [Fact]
    public void Generator_Generates_AssetQueryBuilder_And_AssetDetailResponse()
    {
        var (diagnostics, sources) = RunGenerator("public class Dummy {}");

        var builderSource = sources.FirstOrDefault(s => s.HintName == "AssetQueryBuilder.g.cs");
        Assert.NotNull(builderSource.SourceText);

        var code = builderSource.SourceText.ToString();
        Assert.Contains("public sealed class AssetDetailResponse", code);
        Assert.Contains("public sealed class AssetQueryBuilder : IQueryBuilder<AssetDetailResponse>", code);
        Assert.Contains("public static AssetQueryBuilder WithId(string assetId)", code);
        Assert.Contains("public AssetQueryBuilder WithHolders()", code);
        Assert.Contains("public AssetDetailResponse Build()", code);
    }

    [Fact]
    public void Generator_Generates_IAsyncEnumerable_Extensions()
    {
        var (diagnostics, sources) = RunGenerator("public class Dummy {}");

        var extSource = sources.FirstOrDefault(s => s.HintName == "TesseraClientPaginationExtensions.g.cs");
        Assert.NotNull(extSource.SourceText);

        var code = extSource.SourceText.ToString();
        Assert.Contains("public static async IAsyncEnumerable<Holder> GetAssetHoldersAsyncEnumerable", code);
    }

    [Fact]
    public void Generator_Generates_Tessera_SDK_Events_Observable()
    {
        var (diagnostics, sources) = RunGenerator("public class Dummy {}");

        var rxSource = sources.FirstOrDefault(s => s.HintName == "TesseraEventObservableExtensions.g.cs");
        Assert.NotNull(rxSource.SourceText);

        var code = rxSource.SourceText.ToString();
        Assert.Contains("namespace Tessera.SDK.Events", code);
        Assert.Contains("public static IObservable<TesseraEvent> ToObservable", code);
    }

    [Fact]
    public void Generator_Emits_Warning_On_Breaking_OpenApi_Change()
    {
        // Spec missing required endpoint "/assets/{id}/holders"
        var brokenSpec = @"{
  ""openapi"": ""3.0.3"",
  ""paths"": {
    ""/assets"": {},
    ""/assets/{id}"": {}
  }
}";
        var (diagnostics, _) = RunGenerator("public class Dummy {}", brokenSpec);

        var warning = diagnostics.FirstOrDefault(d => d.Id == "TESS001");
        Assert.NotNull(warning);
        Assert.Equal(DiagnosticSeverity.Warning, warning.Severity);
        Assert.Contains("Required endpoint", warning.GetMessage());
    }

    private sealed class InMemoryAdditionalText : AdditionalText
    {
        private readonly string _path;
        private readonly SourceText _text;

        public InMemoryAdditionalText(string path, string text)
        {
            _path = path;
            _text = SourceText.From(text, Encoding.UTF8);
        }

        public override string Path => _path;
        public override SourceText GetText(System.Threading.CancellationToken cancellationToken = default) => _text;
    }
}
