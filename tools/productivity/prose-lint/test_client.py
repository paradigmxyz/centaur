"""Tests for the prose-lint client."""

import json

import httpx
import pytest
from centaur_tool_prose_lint.client import ProseLintClient


def _score_answer(name: str, score: float, probabilities: list[float]) -> dict:
    labels = ["a", "b", "c", "d"]
    return {
        "type": "score",
        "name": name,
        "score": score,
        "probabilities": [
            {"value": i, "label": labels[i], "probability": p} for i, p in enumerate(probabilities)
        ],
        "confidence": 0.8,
    }


def test_score_sends_rubric_and_normalizes_scores() -> None:
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["path"] = request.url.path
        captured["auth"] = request.headers.get("Authorization")
        captured["payload"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "answers": [
                    _score_answer("wordiness", 0.3, [0.7, 0.3, 0, 0]),
                    _score_answer("mannered_prose", 2.4, [0, 0.1, 0.4, 0.5]),
                    {"type": "refusal", "name": "hedging"},
                    _score_answer("citations", 1.5, [0, 0.5, 0.5, 0]),
                ]
            },
        )

    client = ProseLintClient(api_key="test-key", transport=httpx.MockTransport(handler))
    result = client.score("It's not a tool, it's a movement.", context="What is X?")

    payload = captured["payload"]
    assert captured["path"] == "/v1/decisions"
    assert captured["auth"] == "Bearer test-key"
    assert payload["model"] == "gpt-6-luna"
    assert payload["input"] == (
        "Request:\nWhat is X?\n\nResponse to score:\nIt's not a tool, it's a movement."
    )
    assert [q["name"] for q in payload["questions"]] == [
        "wordiness",
        "mannered_prose",
        "hedging",
        "citations",
    ]
    assert all(q["type"] == "score" and len(q["levels"]) == 4 for q in payload["questions"])

    assert result["score"] == 0.8
    assert result["dimensions"]["wordiness"] == {"score": 0.1, "level": "a", "confidence": 0.8}
    assert result["dimensions"]["mannered_prose"] == {
        "score": 0.8,
        "level": "d",
        "confidence": 0.8,
    }
    assert result["dimensions"]["hedging"] == {"refused": True}
    assert result["dimensions"]["citations"]["score"] == 0.5


def test_client_sends_placeholder_key_for_proxy_replacement(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["auth"] = request.headers.get("Authorization")
        names = [q["name"] for q in json.loads(request.content)["questions"]]
        return httpx.Response(
            200, json={"answers": [_score_answer(n, 0.0, [1, 0, 0, 0]) for n in names]}
        )

    client = ProseLintClient(transport=httpx.MockTransport(handler))
    client.score("Draft")

    assert captured["auth"] == "Bearer OPENAI_API_KEY"


def test_score_surfaces_api_errors() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"error": {"message": "bad levels"}})

    client = ProseLintClient(api_key="test-key", transport=httpx.MockTransport(handler))

    with pytest.raises(RuntimeError, match=r"\(400\): bad levels"):
        client.score("Draft")
