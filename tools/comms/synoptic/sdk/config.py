"""SDK configuration for Twitter API clients."""
# ruff: noqa

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class SDKSettings(BaseSettings):
    """SDK-only settings for Twitter API access.

    These settings can be loaded from environment variables or .env file.
    All settings have sensible defaults and are optional.
    """

    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",  # Ignore extra fields like DATABASE_URL when used standalone
    )

    # API credentials
    SYNOPTIC_API_KEY: str = ""
    SYNOPTIC_BASE_URL: str = "https://api.synoptic.com"

    # Legacy RapidAPI credentials (for backward compatibility)
    RAPIDAPI_KEY: str = ""
    TWITTER241_HOST: str = "twitter241.p.rapidapi.com"
    TWITTER_API45_HOST: str = "twitter-api45.p.rapidapi.com"
    TWITTER_API47_HOST: str = "twitter-api47.p.rapidapi.com"
    TWITTER154_HOST: str = "twitter154.p.rapidapi.com"
    TWTTRAPI_HOST: str = "twttrapi.p.rapidapi.com"

    # Retry settings
    RETRY_MAX_ATTEMPTS: int = Field(default=3, ge=1)
    RETRY_WAIT_MIN: float = Field(default=1.0, ge=0)
    RETRY_WAIT_MAX: float = Field(default=30.0, ge=0)
    RETRY_MULTIPLIER: float = Field(default=2.0, ge=1)


# Global settings instance
settings = SDKSettings()
