package tessera

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"math/rand"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Client is the primary thread-safe entry point for interacting with the Tessera REST API.
type Client struct {
	config     ClientConfig
	httpClient *http.Client
	mu         sync.RWMutex
}

// NewClient constructs and initializes a new Tessera SDK client.
func NewClient(opts ...Option) (*Client, error) {
	cfg := DefaultClientConfig()
	for _, opt := range opts {
		opt(&cfg)
	}

	// Clean base URL trailing slash
	cfg.BaseURL = strings.TrimRight(cfg.BaseURL, "/")

	// Validate base URL
	parsedURL, err := url.Parse(cfg.BaseURL)
	if err != nil || parsedURL.Scheme == "" || parsedURL.Host == "" {
		return nil, fmt.Errorf("invalid base URL %q: must include scheme and host", cfg.BaseURL)
	}

	httpClient := cfg.HTTPClient
	if httpClient == nil {
		httpClient = &http.Client{
			Timeout: cfg.Timeout,
		}
	}

	return &Client{
		config:     cfg,
		httpClient: httpClient,
	}, nil
}

// Config returns a copy of the current client configuration.
func (c *Client) Config() ClientConfig {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.config
}

// executeRequest performs an HTTP request with context propagation, retries, and error handling.
func (c *Client) executeRequest(ctx context.Context, method, endpoint string, query url.Values, body io.Reader) ([]byte, error) {
	fullURL := fmt.Sprintf("%s%s", c.config.BaseURL, endpoint)
	if len(query) > 0 {
		fullURL = fmt.Sprintf("%s?%s", fullURL, query.Encode())
	}

	var lastErr error
	backoff := c.config.InitialBackoff

	for attempt := 0; attempt <= c.config.MaxRetries; attempt++ {
		// Check context cancellation before making the attempt
		if err := ctx.Err(); err != nil {
			return nil, err
		}

		req, err := http.NewRequestWithContext(ctx, method, fullURL, body)
		if err != nil {
			return nil, &NetworkError{Op: "create_request", Err: err}
		}

		// Inject standard headers
		req.Header.Set("User-Agent", "tessera-go/0.1.0")
		req.Header.Set("Accept", "application/json")
		if c.config.AuthToken != "" {
			req.Header.Set("Authorization", "Bearer "+c.config.AuthToken)
		}
		for k, v := range c.config.CustomHeaders {
			req.Header.Set(k, v)
		}

		resp, err := c.httpClient.Do(req)
		if err != nil {
			lastErr = &NetworkError{Op: "http_do", Err: err}
			// Only retry if context hasn't expired and method is idempotent
			if isIdempotent(method) && attempt < c.config.MaxRetries {
				sleepDuration := addJitter(backoff)
				select {
				case <-ctx.Done():
					return nil, ctx.Err()
				case <-time.After(sleepDuration):
					backoff = minDuration(backoff*2, c.config.MaxBackoff)
					continue
				}
			}
			return nil, lastErr
		}

		// Read response body
		respBytes, readErr := io.ReadAll(resp.Body)
		_ = resp.Body.Close()
		if readErr != nil {
			return nil, &NetworkError{Op: "read_body", Err: readErr}
		}

		// Handle 429 Too Many Requests
		if resp.StatusCode == http.StatusTooManyRequests {
			retryAfter := parseRetryAfter(resp.Header.Get("Retry-After"))
			lastErr = &RateLimitError{
				RetryAfter: retryAfter,
				Message:    string(respBytes),
			}
			if attempt < c.config.MaxRetries {
				sleepDuration := retryAfter
				if sleepDuration <= 0 {
					sleepDuration = addJitter(backoff)
				}
				select {
				case <-ctx.Done():
					return nil, ctx.Err()
				case <-time.After(sleepDuration):
					backoff = minDuration(backoff*2, c.config.MaxBackoff)
					continue
				}
			}
			return nil, lastErr
		}

		// Handle 5xx server errors for idempotent operations
		if resp.StatusCode >= 500 && isIdempotent(method) && attempt < c.config.MaxRetries {
			lastErr = &APIError{
				StatusCode: resp.StatusCode,
				ErrorCode:  "server_error",
				Message:    string(respBytes),
			}
			sleepDuration := addJitter(backoff)
			select {
			case <-ctx.Done():
				return nil, ctx.Err()
			case <-time.After(sleepDuration):
				backoff = minDuration(backoff*2, c.config.MaxBackoff)
				continue
			}
		}

		// Check for non-2xx status codes
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			var errBody ApiErrorBody
			if jsonErr := json.Unmarshal(respBytes, &errBody); jsonErr == nil && errBody.Error != "" {
				return nil, &APIError{
					StatusCode: resp.StatusCode,
					ErrorCode:  errBody.Error,
					Message:    errBody.Message,
				}
			}
			return nil, &APIError{
				StatusCode: resp.StatusCode,
				ErrorCode:  http.StatusText(resp.StatusCode),
				Message:    string(respBytes),
			}
		}

		return respBytes, nil
	}

	return nil, lastErr
}

func isIdempotent(method string) bool {
	return method == http.MethodGet || method == http.MethodHead || method == http.MethodOptions
}

func minDuration(a, b time.Duration) time.Duration {
	if a < b {
		return a
	}
	return b
}

func addJitter(d time.Duration) time.Duration {
	jitter := time.Duration(rand.Int63n(int64(d / 4)))
	return d + jitter
}

func parseRetryAfter(header string) time.Duration {
	if header == "" {
		return 0
	}
	if seconds, err := strconv.Atoi(header); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 0
}
