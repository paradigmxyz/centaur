"""Event Registry article client."""

from typing import Literal, TypedDict

import httpx

from centaur_sdk import secret

StringOrList = str | list[str]
EVENT_REGISTRY_DOCS_URL = "https://newsapi.ai/documentation?tab=searchArticles"


class GetArticlesRequest(TypedDict, total=False):
    """Documented request fields for the NewsAPI.ai getArticles endpoint."""

    action: Literal["getArticles"]
    resultType: str
    articlesPage: int
    articlesCount: int
    articlesSortBy: str
    articlesSortByAsc: bool
    articleBodyLen: int
    dataType: StringOrList
    forceMaxDataTimeWindow: int

    query: dict
    keyword: StringOrList
    conceptUri: StringOrList
    categoryUri: StringOrList
    sourceUri: StringOrList
    sourceLocationUri: StringOrList
    sourceGroupUri: StringOrList
    authorUri: StringOrList
    locationUri: StringOrList
    lang: StringOrList
    dateStart: str
    dateEnd: str
    dateMentionStart: str
    dateMentionEnd: str
    keywordLoc: str
    keywordOper: str
    keywordSearchMode: str
    conceptOper: str
    categoryOper: str
    ignoreKeyword: StringOrList
    ignoreConceptUri: StringOrList
    ignoreCategoryUri: StringOrList
    ignoreSourceUri: StringOrList
    ignoreSourceLocationUri: StringOrList
    ignoreSourceGroupUri: StringOrList
    ignoreLocationUri: StringOrList
    ignoreAuthorUri: StringOrList
    ignoreLang: StringOrList
    ignoreKeywordLoc: str
    startSourceRankPercentile: int
    endSourceRankPercentile: int
    minSentiment: float
    maxSentiment: float
    isDuplicateFilter: str
    eventFilter: str

    includeArticleTitle: bool
    includeArticleBasicInfo: bool
    includeArticleBody: bool
    includeArticleEventUri: bool
    includeArticleSocialScore: bool
    includeArticleSentiment: bool
    includeArticleConcepts: bool
    includeArticleCategories: bool
    includeArticleLocation: bool
    includeArticleImage: bool
    includeArticleAuthors: bool
    includeArticleVideos: bool
    includeArticleLinks: bool
    includeArticleExtractedDates: bool
    includeArticleDuplicateList: bool
    includeArticleOriginalArticle: bool
    includeSourceTitle: bool
    includeSourceDescription: bool
    includeSourceLocation: bool
    includeSourceRanking: bool
    includeConceptLabel: bool
    includeConceptImage: bool
    includeConceptSynonyms: bool
    conceptLang: str
    includeCategoryParentUri: bool
    includeLocationPopulation: bool
    includeLocationGeoNamesId: bool
    includeLocationCountryArea: bool
    includeLocationCountryContinent: bool
    includeLocationGeoLocation: bool


DEFAULT_GET_ARTICLES_REQUEST: GetArticlesRequest = {
    "action": "getArticles",
    "keywordSearchMode": "exact",
    "keywordLoc": "body,title",
    "lang": "eng",
    "dataType": ["news"],
    "startSourceRankPercentile": 0,
    "endSourceRankPercentile": 50,
    "isDuplicateFilter": "skipDuplicates",
    "forceMaxDataTimeWindow": 31,
    "articlesCount": 50,
    "articlesSortBy": "rel",
    "articlesSortByAsc": False,
    "includeArticleBody": True,
    "articleBodyLen": 5000,
    "includeArticleAuthors": True,
    "includeSourceTitle": True,
    "includeSourceRanking": True,
    "resultType": "articles",
}


class EventRegistryClient:
    """Client for the Event Registry getArticles endpoint."""

    URL = "https://eventregistry.org/api/v1/article/getArticles"

    def __init__(self, api_key: str | None = None, timeout: float = 30.0):
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _get_api_key(self) -> str:
        api_key = self._api_key or secret("EVENTREGISTRY_API_KEY", "")
        if not api_key:
            raise RuntimeError("EVENTREGISTRY_API_KEY not set.")
        return api_key

    def get_articles(self, request: GetArticlesRequest) -> dict:
        """Submit a getArticles request and return JSON.

        Parameters: https://newsapi.ai/documentation?tab=searchArticles
        """
        unknown = set(request) - set(GetArticlesRequest.__annotations__)
        if unknown:
            raise ValueError(f"Unsupported getArticles fields: {', '.join(sorted(unknown))}")
        if request.get("action", "getArticles") != "getArticles":
            raise ValueError("action must be getArticles")

        body: GetArticlesRequest = {**DEFAULT_GET_ARTICLES_REQUEST, **request}
        body["action"] = "getArticles"

        try:
            response = self.client.post(
                self.URL,
                params={"apiKey": self._get_api_key()},
                json=body,
            )
            response.raise_for_status()
            data = response.json()
        except httpx.HTTPStatusError as exc:
            raise RuntimeError(
                f"Event Registry error: {exc.response.status_code} - {exc.response.text}"
            ) from exc
        except httpx.RequestError as exc:
            raise RuntimeError(f"Event Registry request failed: {exc}") from exc
        except ValueError as exc:
            raise RuntimeError("Event Registry returned a non-JSON response.") from exc

        if not isinstance(data, dict):
            raise RuntimeError("Event Registry returned an unexpected response.")
        if data.get("error"):
            raise RuntimeError(f"Event Registry error: {data['error']}")
        return data

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> "EventRegistryClient":
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


def _client() -> EventRegistryClient:
    return EventRegistryClient()
