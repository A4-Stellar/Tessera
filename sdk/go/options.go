package tessera

import (
	"net/http"
	"time"
)

// Option represents a functional configuration option for the Tessera Client.
type Option func(*ClientConfig)

// ClientConfig holds the resolved configuration for the Client.
type ClientConfig struct {
	BaseURL        string
	WebSocketURL   string
	Timeout        time.Duration
	MaxRetries     int
	InitialBackoff time.Duration
	MaxBackoff     time.Duration
	AuthToken      string
	HTTPClient     *http.Client
	CustomHeaders  map[string]string
}

// DefaultClientConfig returns production-ready default client settings.
func DefaultClientConfig() ClientConfig {
	return ClientConfig{
		BaseURL:        "http://localhost:8080",
		WebSocketURL:   "ws://localhost:8080/v1/ws",
		Timeout:        30 * time.Second,
		MaxRetries:     3,
		InitialBackoff: 200 * time.Millisecond,
		MaxBackoff:     3 * time.Second,
		CustomHeaders:  make(map[string]string),
	}
}

// WithBaseURL sets the base URL for the Tessera REST API.
func WithBaseURL(url string) Option {
	return func(c *ClientConfig) {
		c.BaseURL = url
	}
}

// WithWebSocketURL sets the URL for WebSocket streaming.
func WithWebSocketURL(url string) Option {
	return func(c *ClientConfig) {
		c.WebSocketURL = url
	}
}

// WithTimeout sets the request timeout for standard HTTP operations.
func WithTimeout(d time.Duration) Option {
	return func(c *ClientConfig) {
		c.Timeout = d
	}
}

// WithMaxRetries sets the maximum number of retry attempts for idempotent requests.
func WithMaxRetries(retries int) Option {
	return func(c *ClientConfig) {
		if retries < 0 {
			retries = 0
		}
		c.MaxRetries = retries
	}
}

// WithBackoff sets the exponential backoff parameters for retries.
func WithBackoff(initial, max time.Duration) Option {
	return func(c *ClientConfig) {
		c.InitialBackoff = initial
		c.MaxBackoff = max
	}
}

// WithAuthToken sets the Bearer authentication token for protected routes (e.g. metrics).
func WithAuthToken(token string) Option {
	return func(c *ClientConfig) {
		c.AuthToken = token
	}
}

// WithHTTPClient allows passing a custom *http.Client (e.g. for mTLS or custom transport).
func WithHTTPClient(client *http.Client) Option {
	return func(c *ClientConfig) {
		c.HTTPClient = client
	}
}

// WithCustomHeader adds a custom HTTP header to all outbound requests.
func WithCustomHeader(key, value string) Option {
	return func(c *ClientConfig) {
		if c.CustomHeaders == nil {
			c.CustomHeaders = make(map[string]string)
		}
		c.CustomHeaders[key] = value
	}
}
