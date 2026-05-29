"""Unit tests for sandbox token minting and verification in api.deps."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pytest

import types

from fastapi import HTTPException

from api.deps import (
    enforce_sandbox_thread_scope,
    mint_sandbox_token,
    sandbox_thread_in_scope,
    verify_sandbox_token,
)

_TEST_SECRET = "test-secret-key-for-unit-tests"


@pytest.fixture(autouse=True)
def _set_api_secret_key(monkeypatch):
    monkeypatch.setenv("API_SECRET_KEY", _TEST_SECRET)


class TestRoundtrip:
    def test_mint_then_verify_returns_correct_claims(self):
        token = mint_sandbox_token("thread:1", "ctr-abc")
        claims = verify_sandbox_token(token)
        assert claims is not None
        assert claims["thread_key"] == "thread:1"
        assert claims["container_id"] == "ctr-abc"
        assert "created_at" in claims
        assert "expires_at" in claims
        assert claims["expires_at"] - claims["created_at"] == 7200

    def test_token_has_sbx1_prefix(self):
        token = mint_sandbox_token("t", "c")
        assert token.startswith("sbx1.")


class TestTampering:
    def test_tampered_payload_returns_none(self):
        token = mint_sandbox_token("thread:1", "ctr-abc")
        parts = token.split(".")
        # Flip a character in the payload
        payload = parts[1]
        tampered = payload[:-1] + ("A" if payload[-1] != "A" else "B")
        bad_token = f"{parts[0]}.{tampered}.{parts[2]}"
        assert verify_sandbox_token(bad_token) is None

    def test_tampered_signature_returns_none(self):
        token = mint_sandbox_token("thread:1", "ctr-abc")
        parts = token.split(".")
        sig = parts[2]
        tampered = sig[:-1] + ("A" if sig[-1] != "A" else "B")
        bad_token = f"{parts[0]}.{parts[1]}.{tampered}"
        assert verify_sandbox_token(bad_token) is None


class TestExpiry:
    def test_expired_token_returns_none(self):
        # Mint with ttl=0 — immediately expired
        token = mint_sandbox_token("thread:1", "ctr-abc", ttl_s=0)
        # time.time() > expires_at since expires_at == created_at
        assert verify_sandbox_token(token) is None

    def test_future_token_is_valid(self):
        token = mint_sandbox_token("thread:1", "ctr-abc", ttl_s=3600)
        assert verify_sandbox_token(token) is not None


class TestBadFormat:
    def test_wrong_prefix_returns_none(self):
        assert verify_sandbox_token("bad.payload.sig") is None

    def test_too_few_parts_returns_none(self):
        assert verify_sandbox_token("sbx1.onlyonepart") is None

    def test_too_many_parts_returns_none(self):
        assert verify_sandbox_token("sbx1.a.b.c") is None

    def test_empty_string_returns_none(self):
        assert verify_sandbox_token("") is None


class TestMissingKey:
    def test_verify_without_api_secret_key_returns_none(self, monkeypatch):
        token = mint_sandbox_token("thread:1", "ctr-abc")
        monkeypatch.delenv("API_SECRET_KEY")
        assert verify_sandbox_token(token) is None


class TestDifferentKey:
    def test_token_from_different_key_returns_none(self, monkeypatch):
        token = mint_sandbox_token("thread:1", "ctr-abc")
        monkeypatch.setenv("API_SECRET_KEY", "different-secret-key")
        assert verify_sandbox_token(token) is None


class TestSandboxThreadScope:
    def test_tokens_are_scoped_to_exact_thread(self):
        assert sandbox_thread_in_scope("thread:1", "thread:1")
        assert not sandbox_thread_in_scope("thread:1", "thread:2")
        assert not sandbox_thread_in_scope("thread:1", "thread:1:child")


def _request(sandbox_claims):
    return types.SimpleNamespace(
        state=types.SimpleNamespace(sandbox_claims=sandbox_claims)
    )


class TestEnforceSandboxThreadScope:
    def test_non_sandbox_caller_is_never_restricted(self):
        # No claims → service key; reads and writes to any thread are allowed.
        enforce_sandbox_thread_scope(_request(None), "thread:other", write=True)
        enforce_sandbox_thread_scope(_request(None), "thread:other", write=False)

    def test_own_thread_allowed_for_reads_and_writes(self):
        req = _request({"thread_key": "thread:1"})
        enforce_sandbox_thread_scope(req, "thread:1", write=True)
        enforce_sandbox_thread_scope(req, "thread:1", write=False)

    def test_cross_thread_write_always_rejected(self, monkeypatch):
        # Even with reads relaxed, writes stay confined to the token's thread.
        monkeypatch.setenv("SANDBOX_CROSS_THREAD_READS", "1")
        req = _request({"thread_key": "thread:1"})
        with pytest.raises(HTTPException) as excinfo:
            enforce_sandbox_thread_scope(req, "thread:2", write=True)
        assert excinfo.value.status_code == 403

    def test_cross_thread_read_allowed_by_default(self, monkeypatch):
        monkeypatch.delenv("SANDBOX_CROSS_THREAD_READS", raising=False)
        req = _request({"thread_key": "thread:1"})
        enforce_sandbox_thread_scope(req, "thread:2", write=False)

    def test_cross_thread_read_rejected_when_locked(self, monkeypatch):
        monkeypatch.setenv("SANDBOX_CROSS_THREAD_READS", "0")
        req = _request({"thread_key": "thread:1"})
        with pytest.raises(HTTPException) as excinfo:
            enforce_sandbox_thread_scope(req, "thread:2", write=False)
        assert excinfo.value.status_code == 403
