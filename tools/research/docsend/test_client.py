import importlib.util
from pathlib import Path

import pytest

_CLIENT_SPEC = importlib.util.spec_from_file_location(
    "docsend_client_under_test", Path(__file__).with_name("client.py")
)
assert _CLIENT_SPEC is not None and _CLIENT_SPEC.loader is not None
_CLIENT = importlib.util.module_from_spec(_CLIENT_SPEC)
_CLIENT_SPEC.loader.exec_module(_CLIENT)
_normalize_verification_url = _CLIENT._normalize_verification_url


@pytest.mark.parametrize(
    "url",
    [
        "https://docsend.com/presentation_users/token",
        "https://paradigm.docsend.com/presentation_users/token",
        "https://track.pstmrk.it/3s/docsend.com%2Fpresentation_users%2Ftoken/abc",
    ],
)
def test_normalize_verification_url_allows_supported_hosts(url: str) -> None:
    assert _normalize_verification_url(url) == url


@pytest.mark.parametrize(
    "url",
    [
        "http://track.pstmrk.it/3s/docsend.com%2Fpresentation_users%2Ftoken/abc",
        "https://pstmrk.it/3s/docsend.com%2Fpresentation_users%2Ftoken/abc",
        "https://track.pstmrk.it.evil.example/3s/docsend.com",
        "https://example.com/presentation_users/token",
    ],
)
def test_normalize_verification_url_rejects_unsupported_urls(url: str) -> None:
    with pytest.raises(ValueError, match="HTTPS DocSend or Postmark URL"):
        _normalize_verification_url(url)
