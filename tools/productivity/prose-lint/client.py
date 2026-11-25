"""Lint drafts for wordiness, mannered prose, hedging, and citations with the OpenAI Decisions API."""

from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://api.openai.com/v1"
MODEL = "gpt-6-luna"

# Each dimension is a Decisions API score question. Levels run from clean
# (index 0) to worst (last index); scores are normalized to 0-1.
DIMENSIONS: list[dict[str, Any]] = [
    {
        "name": "wordiness",
        "instructions": (
            "Does the response use more words than its content needs? Count as "
            "padding: preamble, restating the question, methodology or checks "
            "nobody asked for, secondary examples, extra sources, closing lines "
            "that repeat the opening, and wordy phrases such as 'in order to' or "
            "'there are a number of'. Judge length against what was asked."
        ),
        "levels": [
            ("Tight", "Every sentence is an answer, reason, number, or check. Nothing to cut."),
            ("Mostly tight", "A few wordy phrases could be shorter, but no sentence is filler."),
            (
                "Padded",
                "Has filler sentences; could lose about a quarter of its words without losing meaning.",
            ),
            ("Bloated", "The answer is buried; it could be said in under half the words."),
        ],
    },
    {
        "name": "mannered_prose",
        "instructions": (
            "Does the response use mannered, AI-sounding prose instead of plain "
            "words? Tics include: 'not X, it's Y' or 'isn't just X, it's Y' "
            "contrasts; rhetorical questions it answers itself; triplets for "
            "rhythm; dramatic colon or em-dash reveals; stock openers and closers "
            "('Great question', 'Here's the thing', 'Let's dive in', 'The real "
            "question is', 'In short'); grand framing ('In today's rapidly "
            "evolving landscape'); and inflated words (delve, crucial, robust, "
            "landscape, tapestry, seamless, game-changer, unlock)."
        ),
        "levels": [
            ("Plain", "Reads like a sharp colleague messaging a partner. No tics."),
            ("One tic", "One mannered construction or inflated word in otherwise plain prose."),
            ("Several tics", "Several tics; the prose noticeably sounds AI-written."),
            ("Pervasive", "Tics drive the structure; most sentences are mannered."),
        ],
    },
    {
        "name": "hedging",
        "instructions": (
            "Does the response hedge instead of committing to a position? Count "
            "as hedging: caveats that would not change the decision, repeated "
            "caveats, stacked qualifiers ('may', 'could potentially'), wide "
            "ranges instead of a base case, labels like 'illustrative' or "
            "'provisional', and phrases such as 'doesn't establish', 'my inference is', 'not an "
            "underwritten', or 'starting hypothesis'. One caveat that would "
            "change the decision is not hedging."
        ),
        "levels": [
            ("Committed", "Takes a clear position with its main reason. No needless caveats."),
            ("Measured", "Takes a position; adds a minor caveat or qualifier it could drop."),
            ("Hedged", "The position is diluted by several caveats or qualifiers."),
            ("Evasive", "Avoids a position; mostly caveats, ranges, and disclaimers."),
        ],
    },
    {
        "name": "citations",
        "instructions": (
            "How many sources does the response cite or discuss? Count each "
            "distinct source named, linked, or attributed (such as 'IDC 2025' or "
            "'per Bloomberg'). Lists of sources consulted, narrating the research "
            "('pieced together from', 'based on my research across'), and notes "
            "about sources, files, or channels it could not access all count as "
            "discussing sources. Links to the response's own attached files or "
            "artifacts are not sources."
        ),
        "levels": [
            ("None", "Cites or discusses no sources."),
            ("One source", "Cites a single source."),
            ("A few sources", "Cites two or three sources, or notes a source it could not access."),
            (
                "Many sources",
                "Cites four or more sources, lists sources consulted, or narrates its research.",
            ),
        ],
    },
]


class ProseLintClient:
    """Score text for wordiness, mannered prose, hedging, and citations."""

    def __init__(
        self,
        api_key: str | None = None,
        base_url: str = BASE_URL,
        timeout: float = 30.0,
        transport: httpx.BaseTransport | None = None,
    ):
        # Without a key, send the secret name as a placeholder; iron-proxy
        # replaces it with the real key on requests to api.openai.com.
        key = api_key or secret("OPENAI_API_KEY", "OPENAI_API_KEY")
        self._http = httpx.Client(
            base_url=base_url.rstrip("/"),
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}"},
            timeout=timeout,
            transport=transport,
        )

    def score(
        self,
        text: str,
        context: str | None = None,
    ) -> dict[str, Any]:
        """Score a draft for wordiness, mannered prose, hedging, and citations.

        Scores run from 0 (clean) to 1 (worst). Returns ``score`` (the highest
        dimension score) and per-dimension ``score``, ``level``, and
        ``confidence``. Callers choose their own rewrite threshold.

        Args:
            text: The exact draft to score.
            context: Optional request the draft answers; improves wordiness and
                hedging judgments.
        """
        if not text.strip():
            raise ValueError("text must not be empty.")

        evidence = f"Response to score:\n{text}"
        if context and context.strip():
            evidence = f"Request:\n{context.strip()}\n\n{evidence}"

        answers = self._decide(
            {
                "model": MODEL,
                "input": evidence,
                "questions": [
                    {
                        "type": "score",
                        "name": dim["name"],
                        "instructions": dim["instructions"],
                        "levels": [
                            {"label": label, "description": description}
                            for label, description in dim["levels"]
                        ],
                    }
                    for dim in DIMENSIONS
                ],
            }
        )
        by_name = {answer.get("name"): answer for answer in answers}

        dimensions: dict[str, Any] = {}
        for dim in DIMENSIONS:
            answer = by_name.get(dim["name"]) or {}
            if answer.get("type") != "score":
                dimensions[dim["name"]] = {"refused": True}
                continue
            score = round(answer["score"] / (len(dim["levels"]) - 1), 3)
            top = max(answer.get("probabilities") or [], key=lambda p: p["probability"], default={})
            dimensions[dim["name"]] = {
                "score": score,
                "level": top.get("label"),
                "confidence": answer.get("confidence"),
            }

        scores = [d["score"] for d in dimensions.values() if "score" in d]
        if not scores:
            raise RuntimeError("Decisions API refused to score every dimension.")
        return {"score": max(scores), "dimensions": dimensions}

    def _decide(self, payload: dict[str, Any]) -> list[dict[str, Any]]:
        try:
            response = self._http.post("/decisions", json=payload)
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            raise RuntimeError(
                f"OpenAI Decisions API error ({exc.response.status_code}): "
                f"{_error_detail(exc.response)}"
            ) from exc
        except httpx.RequestError as exc:
            raise RuntimeError(f"OpenAI Decisions request failed: {exc}") from exc
        return response.json().get("answers") or []

    def close(self) -> None:
        """Close the HTTP client."""
        self._http.close()


def _error_detail(response: httpx.Response) -> Any:
    try:
        payload = response.json()
    except ValueError:
        return response.text
    error = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(error, dict):
        return error.get("message", error)
    return error or payload


def _client() -> ProseLintClient:
    return ProseLintClient()
