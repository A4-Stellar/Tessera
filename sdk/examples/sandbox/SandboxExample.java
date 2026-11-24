// Java sandbox example — production client, sandbox tenant header.
import io.tessera.sdk.TesseraClient;
import io.tessera.sdk.TesseraClientConfig;

import java.net.URI;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;

public final class SandboxExample {
    public static void main(String[] args) throws Exception {
        String api = System.getenv().getOrDefault("TESSERA_SANDBOX_URL", "http://localhost:8081");
        String tenant = System.getenv().getOrDefault("X_SANDBOX_TENANT_ID", "acme-1");

        TesseraClient client = TesseraClient.create(
                TesseraClientConfig.builder()
                        .baseUri(URI.create(api))
                        .defaultHeader("X-Sandbox-Tenant-ID", tenant)
                        .build());

        // Identical call shape to production.
        System.out.println("assets=" + client.getStats().totalAssets());

        // Sandbox-only reset uses the same base URL.
        var request = HttpRequest.newBuilder(URI.create(api + "/sandbox/reset"))
                .header("X-Sandbox-Tenant-ID", tenant)
                .POST(HttpRequest.BodyPublishers.noBody())
                .build();
        HttpResponse<String> response = java.net.http.HttpClient.newHttpClient()
                .send(request, HttpResponse.BodyHandlers.ofString());
        System.out.println("reset status=" + response.statusCode());
    }
}
