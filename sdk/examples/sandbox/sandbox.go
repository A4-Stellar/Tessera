// Go sandbox example — production SDK, sandbox tenant header.
package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"time"

	tessera "github.com/A4-Stellar/Tessera/sdk/go"
)

func main() {
	api := os.Getenv("TESSERA_SANDBOX_URL")
	if api == "" {
		api = "http://localhost:8081"
	}
	tenant := os.Getenv("X_SANDBOX_TENANT_ID")
	if tenant == "" {
		tenant = "acme-1"
	}

	client, err := tessera.NewClient(
		tessera.WithBaseURL(api),
		tessera.WithTimeout(15*time.Second),
		tessera.WithDefaultHeaders(http.Header{"X-Sandbox-Tenant-ID": []string{tenant}}),
	)
	if err != nil {
		panic(err)
	}

	ctx := context.Background()
	stats, err := client.GetStats(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Printf("tenant=%s assets=%d\n", tenant, stats.TotalAssets)

	// Sandbox-only lifecycle call uses the same HTTP surface.
	req, _ := http.NewRequestWithContext(ctx, http.MethodPost, api+"/sandbox/reset", nil)
	req.Header.Set("X-Sandbox-Tenant-ID", tenant)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		panic(err)
	}
	defer resp.Body.Close()
	fmt.Println("reset status:", resp.StatusCode)
}
