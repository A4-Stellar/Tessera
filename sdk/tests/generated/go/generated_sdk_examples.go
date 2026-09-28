package generated_test

import (
	"testing"
	"github.com/stretchr/testify/assert"
)

const apiBase = "http://localhost:8080"

func TestGoExample1_all(t *testing.T) {
	client, err := tessera.NewClient("http://localhost:8080")
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()

	assets, err := client.GetAssets(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	for _, asset := range assets {
		fmt.Printf("Asset: %s (ID: %d)\n", asset.Name, asset.ID)
	}
}

func TestGoExample2_all(t *testing.T) {
	compliance := tessera.NewContract("CBUERYDM…D2IU")
	op := compliance.Call(
		"add_to_allowlist",
		adminAddress.ToScVal(),
		investorAddress.ToScVal(),
		tessera.NativeToScVal("US", tessera.StringType),
		tessera.NativeToScVal(currentLedger+6300000, tessera.U32Type),
	)
}
