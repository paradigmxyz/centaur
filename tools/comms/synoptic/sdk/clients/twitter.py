"""Twitter client that extends SynopticClient.

This is the main client class for the Twitter SDK. It inherits from SynopticClient
and can be extended with additional providers and features in the future.
"""
# ruff: noqa

from .synoptic import (
    ApiUsage,
    RetryConfig,
    SynopticClient,
    UnitsLimitExceededError,
)

__all__ = [
    "TwitterClient",
    "ApiUsage",
    "RetryConfig",
    "UnitsLimitExceededError",
]


class TwitterClient(SynopticClient):
    """Main Twitter API client.

    Currently uses Synoptic as the backend provider. Can be extended
    with additional providers and features in the future.

    Examples:
        # Basic usage
        client = TwitterClient(api_key="your-key")
        user = await client.get_user_by_screen_name("elonmusk")

        # Context manager usage (recommended)
        async with TwitterClient(api_key="your-key") as client:
            user = await client.get_user_by_screen_name("elonmusk")
            followers, cursor, meta = await client.get_followers("elonmusk", ids_only=True)
            tweets, cursor, meta = await client.search_tweets("bitcoin")
    """

    pass  # Inherits all methods from SynopticClient
