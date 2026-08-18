"""API response models for Twitter SDK."""
# ruff: noqa

from enum import Enum

from pydantic import BaseModel


class Provider(str, Enum):
    """Available Twitter API providers."""

    TWITTER241 = "twitter241"
    TWITTER154 = "twitter154"
    TWITTER_API45 = "twitter_api45"
    TWITTER_API47 = "twitter_api47"
    TWTTRAPI = "twttrapi"
    SYNOPTIC_TWTTR = "synoptic_twttr"


class Follower(BaseModel):
    """Model for a follower user."""

    id: str | None = None
    username: str | None = None
    name: str | None = None
    description: str | None = None
    followers_count: int | None = None
    following_count: int | None = None


class FollowersResponse(BaseModel):
    """Response model for followers endpoint."""

    username: str
    provider: Provider
    followers: list[Follower]
    next_cursor: str | None = None
    count: int


class Following(BaseModel):
    """Model for a following user."""

    id: str | None = None
    username: str | None = None
    name: str | None = None
    description: str | None = None
    followers_count: int | None = None
    following_count: int | None = None


class FollowingResponse(BaseModel):
    """Response model for following endpoint."""

    username: str
    provider: Provider
    following: list[Following]
    next_cursor: str | None = None
    count: int
