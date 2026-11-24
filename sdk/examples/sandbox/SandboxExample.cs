// C# sandbox example — production SDK, sandbox tenant header.
using System;
using System.Net.Http;
using System.Threading.Tasks;
using Tessera.SDK;

public static class SandboxExample
{
    public static async Task Main()
    {
        var api = Environment.GetEnvironmentVariable("TESSERA_SANDBOX_URL")
                  ?? "http://localhost:8081";
        var tenant = Environment.GetEnvironmentVariable("X_SANDBOX_TENANT_ID")
                     ?? "acme-1";

        using var client = TesseraClient.Create(new TesseraClientOptions
        {
            BaseAddress = new Uri(api),
            DefaultHeaders = { ["X-Sandbox-Tenant-ID"] = tenant },
        });

        // Identical call shape to production.
        var stats = await client.GetStatsAsync();
        Console.WriteLine($"assets={stats.TotalAssets}");

        // Sandbox-only reset.
        using var http = new HttpClient();
        using var request = new HttpRequestMessage(HttpMethod.Post, $"{api}/sandbox/reset");
        request.Headers.Add("X-Sandbox-Tenant-ID", tenant);
        using var response = await http.SendAsync(request);
        Console.WriteLine($"reset status={(int)response.StatusCode}");
    }
}
