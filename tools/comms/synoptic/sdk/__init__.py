"""Twitter SDK - Lightweight Twitter API client library."""
# ruff: noqa

from .clients.synoptic import (
    BUDGET_EXCEEDED_ERROR,
    ApiUsage,
    SynopticClient,
    UnitsLimitExceededError,
)
from .clients.twitter import TwitterClient
from .config import SDKSettings, settings
from .exceptions import (
    RetryableHTTPError,
    TwitterSDKError,
)
from .models import (
    Follower,
    FollowersResponse,
    Following,
    FollowingResponse,
    Provider,
)

__all__ = [
    # Client (primary)
    "TwitterClient",
    # Client (low-level, for backward compatibility)
    "SynopticClient",
    "ApiUsage",
    "BUDGET_EXCEEDED_ERROR",
    "UnitsLimitExceededError",
    # Config
    "SDKSettings",
    "settings",
    # Exceptions
    "RetryableHTTPError",
    "TwitterSDKError",
    # Models
    "Follower",
    "Following",
    "FollowersResponse",
    "FollowingResponse",
    "Provider",
]
