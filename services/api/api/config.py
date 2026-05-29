from __future__ import annotations

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="", env_file=".env", extra="ignore")

    database_url: str = Field(alias="DATABASE_URL")
    cors_origins: list[str] = []
    cors_origin_regex: str | None = None

    @property
    def cors_allow_origin_regex(self) -> str | None:
        if self.cors_origin_regex == "*":
            return ".*"
        return self.cors_origin_regex


settings = Settings()
