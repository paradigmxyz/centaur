import pytest
from tools.research.docsend.client import _decode_session_url, _encode_session_url


def test_session_url_metadata_is_alphanumeric_and_round_trips() -> None:
    url = "https://paradigm.docsend.com/view/gsr4rfcmm6ptpbir"

    encoded = _encode_session_url(url)

    assert encoded.isalnum()
    assert _decode_session_url(encoded) == url


def test_session_url_decoder_accepts_legacy_raw_urls() -> None:
    url = "https://docsend.com/view/example"

    assert _decode_session_url(url) == url


def test_session_url_decoder_rejects_invalid_encoded_values() -> None:
    with pytest.raises(ValueError, match="Invalid DocSend URL"):
        _decode_session_url("hexnothex")
