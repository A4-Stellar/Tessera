package tessera

import (
	"fmt"
	"time"
)

// APIError represents an error response returned by the Tessera REST API.
type APIError struct {
	StatusCode int
	ErrorCode  string
	Message    string
}

func (e *APIError) Error() string {
	return fmt.Sprintf("tessera api error (status %d, code %s): %s", e.StatusCode, e.ErrorCode, e.Message)
}

// RateLimitError is returned when the API responds with HTTP 429 Too Many Requests.
type RateLimitError struct {
	RetryAfter time.Duration
	Message    string
}

func (e *RateLimitError) Error() string {
	if e.RetryAfter > 0 {
		return fmt.Sprintf("tessera rate limit exceeded: %s (retry after %s)", e.Message, e.RetryAfter)
	}
	return fmt.Sprintf("tessera rate limit exceeded: %s", e.Message)
}

// NetworkError represents a low-level network failure, connection drop, or timeout.
type NetworkError struct {
	Op  string
	Err error
}

func (e *NetworkError) Error() string {
	return fmt.Sprintf("tessera network error during %s: %v", e.Op, e.Err)
}

func (e *NetworkError) Unwrap() error {
	return e.Err
}
