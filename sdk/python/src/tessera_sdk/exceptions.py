from typing import Any

import httpx


class TesseraError(Exception):
    pass


class APIError(TesseraError):
    def __init__(
        self,
        message: str,
        *,
        status_code: int,
        error_code: str | None = None,
        body: Any = None,
        response_text: str = "",
        retry_after: float | None = None,
        request: httpx.Request | None = None,
    ) -> None:
        self.message = message
        self.status_code = status_code
        self.error_code = error_code
        self.body = body
        self.response_text = response_text
        self.retry_after = retry_after
        self.request = request
        detail = f"HTTP {status_code}: {message}"
        if error_code:
            detail = f"{detail} ({error_code})"
        super().__init__(detail)


class NotFoundError(APIError):
    pass


class RateLimitError(APIError):
    pass


class ConnectionError(TesseraError):
    def __init__(self, message: str, *, request: httpx.Request | None = None) -> None:
        self.message = message
        self.request = request
        super().__init__(f"Connection error: {message}")


class ResponseError(TesseraError):
    def __init__(
        self,
        message: str,
        *,
        status_code: int,
        response_text: str = "",
        request: httpx.Request | None = None,
    ) -> None:
        self.message = message
        self.status_code = status_code
        self.response_text = response_text
        self.request = request
        super().__init__(f"HTTP {status_code}: {message}")


TesseraAPIError = APIError
TesseraNotFoundError = NotFoundError
TesseraRateLimitError = RateLimitError
TesseraConnectionError = ConnectionError
TesseraResponseError = ResponseError
TesseraHTTPError = APIError
APIConnectionError = ConnectionError
APIResponseError = ResponseError


__all__ = [
    "APIConnectionError",
    "APIError",
    "APIResponseError",
    "ConnectionError",
    "NotFoundError",
    "RateLimitError",
    "ResponseError",
    "TesseraAPIError",
    "TesseraConnectionError",
    "TesseraError",
    "TesseraHTTPError",
    "TesseraNotFoundError",
    "TesseraRateLimitError",
    "TesseraResponseError",
]
