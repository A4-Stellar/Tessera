package generated_test

import (
	"testing"
	"github.com/stretchr/testify/assert"
)

const apiBase = "http://localhost:8080"

func TestGoExample1_integration(t *testing.T) {
	// Source: docs/content/en/docs/integration.mdx:84
	package main
	
	import (
		"context"
		"fmt"
		"log"
	
		"github.com/A4-Stellar/tessera-sdk-go"
	)
	
	func main() {
		client, err := tessera.NewClient("http://localhost:8080")
		if err != nil {
			log.Fatal(err)
		}
		defer client.Close()
	
		assets, err := client.GetAssets(context.Background())
		if err != nil {
			log.Fatal(err)
		}
		for _, asset := range assets {
			fmt.Printf("Asset: %s (ID: %d)\n", asset.Name, asset.ID)
		}
	}
}

