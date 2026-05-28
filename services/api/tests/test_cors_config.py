import importlib
import uuid

import pytest


def test_settings_reads_cors_origin_regex_from_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(
        "DATABASE_URL", "postgresql://centaur:centaur@localhost:5432/centaur"
    )
    origin_regex = rf"^https://{uuid.uuid4().hex}\.example\.invalid$"
    monkeypatch.setenv("CORS_ORIGIN_REGEX", origin_regex)

    config = importlib.import_module("api.config")
    settings = config.Settings()

    assert settings.cors_origin_regex == origin_regex
    assert settings.cors_allow_origin_regex == origin_regex


def test_settings_normalizes_wildcard_cors_origin_regex(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(
        "DATABASE_URL", "postgresql://centaur:centaur@localhost:5432/centaur"
    )
    monkeypatch.setenv("CORS_ORIGIN_REGEX", "*")

    config = importlib.import_module("api.config")
    settings = config.Settings()

    assert settings.cors_origin_regex == "*"
    assert settings.cors_allow_origin_regex == ".*"
