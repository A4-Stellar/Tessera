//! Error types for the Tessera SDK.
//!
//! Provides a structured error hierarchy that distinguishes between
//! API errors, rate limiting, network failures, and serialization issues.

use std::fmt;
use std::time::Duration;

/// Errors returned by the Tessera SDK.
#[derive(Debug)]
pub enum TesseraError {
    /// The API returned an error response.
    Api(ApiErrorDetail),

    /// Rate limited by the server (HTTP 429).
    RateLimited {
        /// Seconds to wait before retrying (from `Retry-After` header).
        retry_after: Option<Duration>,
        /// Human-readable message from the server.
        message: String,
    },

    /// Resource not found (HTTP 404).
    NotFound {
        /// The resource identifier that was not found.
        resource: String,
    },

    /// Bad request (HTTP 400).
    BadRequest {
        /// The validation error message.
        message: String,
    },

    /// Authentication or authorization failure (HTTP 401/403).
    Auth {
        /// The auth error message.
        message: String,
    },

    /// Network-level connection failure.
    Connection {
        /// The underlying error message.
        message: String,
        /// Whether the request can be retried.
        retryable: bool,
    },

    /// Request timed out.
    Timeout {
        /// The timeout duration that was exceeded.
        duration: Duration,
    },

    /// Response deserialization failed.
    Serialization {
        /// The deserialization error message.
        message: String,
        /// The raw response body that failed to parse.
        body: String,
    },

    /// Invalid URL or configuration.
    InvalidUrl {
        /// The invalid URL string.
        url: String,
    },

    /// Maximum retry attempts exhausted.
    RetryExhausted {
        /// Number of attempts made.
        attempts: u32,
        /// The last error encountered.
        last_error: Box<TesseraError>,
    },
}

/// Detailed API error information.
#[derive(Debug, Clone)]
pub struct ApiErrorDetail {
    /// HTTP status code.
    pub status: u16,
    /// Error code string (e.g., "not_found", "bad_request").
    pub error: String,
    /// Human-readable error message.
    pub message: String,
}

impl fmt::Display for TesseraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TesseraError::Api(detail) => {
                write!(
                    f,
                    "API error ({}): {} — {}",
                    detail.status, detail.error, detail.message
                )
            }
            TesseraError::RateLimited { retry_after, message } => {
                match retry_after {
                    Some(d) => write!(f, "Rate limited: {} (retry after {:?})", message, d),
                    None => write!(f, "Rate limited: {}", message),
                }
            }
            TesseraError::NotFound { resource } => {
                write!(f, "Not found: {}", resource)
            }
            TesseraError::BadRequest { message } => {
                write!(f, "Bad request: {}", message)
            }
            TesseraError::Auth { message } => {
                write!(f, "Authentication error: {}", message)
            }
            TesseraError::Connection { message, retryable } => {
                write!(f, "Connection error: {} (retryable: {})", message, retryable)
            }
            TesseraError::Timeout { duration } => {
                write!(f, "Request timed out after {:?}", duration)
            }
            TesseraError::Serialization { message, .. } => {
                write!(f, "Serialization error: {}", message)
            }
            TesseraError::InvalidUrl { url } => {
                write!(f, "Invalid URL: {}", url)
            }
            TesseraError::RetryExhausted { attempts, last_error } => {
                write!(
                    f,
                    "Retry exhausted after {} attempts. Last error: {}",
                    attempts, last_error
                )
            }
        }
    }
}

impl std::error::Error for TesseraError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TesseraError::RetryExhausted { last_error, .. } => Some(last_error),
            _ => None,
        }
    }
}

impl TesseraError {
    /// Returns `true` if the error is potentially transient and the request
    /// can be retried.
    pub fn is_retryable(&self) -> bool {
        match self {
            TesseraError::RateLimited { .. } => true,
            TesseraError::Connection { retryable, .. } => *retryable,
            TesseraError::Timeout { .. } => true,
            TesseraError::Api(detail) => {
                // Retry on server errors (5xx)
                detail.status >= 500
            }
            TesseraError::RetryExhausted { .. } => false,
            _ => false,
        }
    }

    /// Returns the API error detail if this is an API error.
    pub fn api_detail(&self) -> Option<&ApiErrorDetail> {
        match self {
            TesseraError::Api(detail) => Some(detail),
            _ => None,
        }
    }
}

// Conversion from reqwest::Error
impl From<reqwest::Error> for TesseraError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            TesseraError::Timeout {
                duration: Duration::from_secs(30), // Default, overridden by config
            }
        } else if err.is_connect() {
            TesseraError::Connection {
                message: err.to_string(),
                retryable: true,
            }
        } else if err.is_decode() {
            TesseraError::Serialization {
                message: err.to_string(),
                body: String::new(),
            }
        } else {
            TesseraError::Connection {
                message: err.to_string(),
                retryable: false,
            }
        }
    }
}

/// Result type alias for Tessera SDK operations.
pub type Result<T> = std::result::Result<T, TesseraError>;
