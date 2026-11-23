package generated_test

import (
	"testing"
	"github.com/stretchr/testify/assert"
)

const apiBase = "http://localhost:8080"

func TestGoExample1_webhook-verification(t *testing.T) {
	// Source: docs/content/en/docs/webhook-verification.mdx:162
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

