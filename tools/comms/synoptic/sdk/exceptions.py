"""Custom exceptions for Twitter SDK."""
# ruff: noqa


class TwitterSDKError(Exception):
    """Base exception for Twitter SDK errors."""

    pass


class RetryableHTTPError(TwitterSDKError):
    """Wrapper for retryable HTTP errors.

    This exception is raised when an HTTP request fails with a retryable
    error (5xx status codes, 429 rate limit, connection/timeout errors).
    """

    def __init__(self, original: Exception):
        self.original = original
        super().__init__(str(original))


class APIError(TwitterSDKError):
    """Error returned by the Twitter API."""

    def __init__(self, message: str, status_code: int | None = None):
        self.status_code = status_code
        super().__init__(message)


class RateLimitError(TwitterSDKError):
    """Rate limit exceeded error."""

    def __init__(self, message: str = "Rate limit exceeded", retry_after: int | None = None):
        self.retry_after = retry_after
        super().__init__(message)


class AuthenticationError(TwitterSDKError):
    """Authentication failed error."""

    pass
