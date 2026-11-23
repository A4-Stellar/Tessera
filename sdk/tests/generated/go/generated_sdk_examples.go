package generated_test

import (
	"testing"
	"github.com/stretchr/testify/assert"
)

const apiBase = "http://localhost:8080"

func TestGoExample1_all(t *testing.T) {
	// Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\integration.mdx:84
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

func TestGoExample2_all(t *testing.T) {
	// Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\webhook-verification.mdx:162
	package main
	
	import (
		"crypto/hmac"
		"crypto/sha256"
		"encoding/hex"
		"io"
		"net/http"
		"os"
		"strings"
	)
	
	func verifyWebhook(secret string, payload []byte, signatureHeaders []string) bool {
		mac := hmac.New(sha256.New, []byte(secret))
		mac.Write(payload)
		expected := hex.EncodeToString(mac.Sum(nil))
	
		for _, header := range signatureHeaders {
			for _, part := range strings.Split(header, ",") {
				if strings.HasPrefix(part, "sha256=") {
					sig := strings.TrimPrefix(part, "sha256=")
					if hmac.Equal([]byte(sig), []byte(expected)) {
						return true
					}
				}
			}
		}
		return false
	}
	
	func webhookHandler(w http.ResponseWriter, r *http.Request) {
		payload, _ := io.ReadAll(r.Body)
		signatures := r.Header.Values("X-Tessera-Signature")
	
		if !verifyWebhook(os.Getenv("TESSERA_WEBHOOK_SECRET"), payload, signatures) {
			http.Error(w, "Invalid signature", http.StatusUnauthorized)
			return
		}
		// Process event...
		w.WriteHeader(http.StatusOK)
	}
}

