using System.Text.Json;
using System.Text.Json.Serialization;

namespace Tessera.SDK;

/// <summary>Serializer settings matching the Tessera API's snake_case JSON.</summary>
public static class TesseraJson
{
    /// <summary>Shared, thread-safe options used for every request and response.</summary>
    public static JsonSerializerOptions Options { get; } = Create();

    private static JsonSerializerOptions Create()
    {
        var options = new JsonSerializerOptions(JsonSerializerDefaults.Web)
        {
            PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
            DictionaryKeyPolicy = JsonNamingPolicy.SnakeCaseLower,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        };
        options.MakeReadOnly(populateMissingResolver: true);
        return options;
    }
}
