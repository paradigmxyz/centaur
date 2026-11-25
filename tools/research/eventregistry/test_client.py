import json
import sys
import tomllib
from pathlib import Path

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).parent))

from client import (
    DEFAULT_GET_ARTICLES_REQUEST,
    EventRegistryClient,
    GetArticlesRequest,
)


def test_get_articles_posts_json_with_requested_defaults() -> None:
    seen: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["method"] = request.method
        seen["path"] = request.url.path
        seen["params"] = request.url.params
        seen["json"] = json.loads(request.content)
        return httpx.Response(200, json={"articles": {"results": []}})

    client = EventRegistryClient(api_key="test-key")
    client._client = httpx.Client(transport=httpx.MockTransport(handler))

    assert client.get_articles({"keyword": "bitcoin"}) == {"articles": {"results": []}}
    assert seen == {
        "method": "POST",
        "path": "/api/v1/article/getArticles",
        "params": httpx.QueryParams({"apiKey": "test-key"}),
        "json": {**DEFAULT_GET_ARTICLES_REQUEST, "keyword": "bitcoin"},
    }


def test_request_type_contains_every_documented_get_articles_field() -> None:
    assert set(GetArticlesRequest.__annotations__) == {
        "action",
        "resultType",
        "articlesPage",
        "articlesCount",
        "articlesSortBy",
        "articlesSortByAsc",
        "articleBodyLen",
        "dataType",
        "forceMaxDataTimeWindow",
        "query",
        "keyword",
        "conceptUri",
        "categoryUri",
        "sourceUri",
        "sourceLocationUri",
        "sourceGroupUri",
        "authorUri",
        "locationUri",
        "lang",
        "dateStart",
        "dateEnd",
        "dateMentionStart",
        "dateMentionEnd",
        "keywordLoc",
        "keywordOper",
        "keywordSearchMode",
        "conceptOper",
        "categoryOper",
        "ignoreKeyword",
        "ignoreConceptUri",
        "ignoreCategoryUri",
        "ignoreSourceUri",
        "ignoreSourceLocationUri",
        "ignoreSourceGroupUri",
        "ignoreLocationUri",
        "ignoreAuthorUri",
        "ignoreLang",
        "ignoreKeywordLoc",
        "startSourceRankPercentile",
        "endSourceRankPercentile",
        "minSentiment",
        "maxSentiment",
        "isDuplicateFilter",
        "eventFilter",
        "includeArticleTitle",
        "includeArticleBasicInfo",
        "includeArticleBody",
        "includeArticleEventUri",
        "includeArticleSocialScore",
        "includeArticleSentiment",
        "includeArticleConcepts",
        "includeArticleCategories",
        "includeArticleLocation",
        "includeArticleImage",
        "includeArticleAuthors",
        "includeArticleVideos",
        "includeArticleLinks",
        "includeArticleExtractedDates",
        "includeArticleDuplicateList",
        "includeArticleOriginalArticle",
        "includeSourceTitle",
        "includeSourceDescription",
        "includeSourceLocation",
        "includeSourceRanking",
        "includeConceptLabel",
        "includeConceptImage",
        "includeConceptSynonyms",
        "conceptLang",
        "includeCategoryParentUri",
        "includeLocationPopulation",
        "includeLocationGeoNamesId",
        "includeLocationCountryArea",
        "includeLocationCountryContinent",
        "includeLocationGeoLocation",
    }


def test_get_articles_rejects_undocumented_fields() -> None:
    client = EventRegistryClient(api_key="test-key")

    with pytest.raises(ValueError, match="Unsupported getArticles fields: typo"):
        client.get_articles({"typo": True})  # type: ignore[typeddict-unknown-key]


def test_manifest_injects_key_for_event_registry() -> None:
    manifest = tomllib.loads(Path(__file__).with_name("pyproject.toml").read_text())

    assert manifest["tool"]["centaur"]["secrets"] == [
        {
            "type": "http",
            "name": "EVENTREGISTRY_API_KEY",
            "mode": "inject",
            "inject_query_param": "apiKey",
            "hosts": ["eventregistry.org"],
        }
    ]
