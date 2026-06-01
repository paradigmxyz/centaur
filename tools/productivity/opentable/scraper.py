"""OpenTable scraper using browser-use."""

import ast
import asyncio
import json
import re
from datetime import datetime
from urllib.parse import urlencode


def build_search_url(
    term: str = "",
    covers: int = 2,
    date_time: str | None = None,
    metro_id: int = 4,
    region_ids: list[int] | None = None,
    neighborhood_ids: list[int] | None = None,
    sort_by: str = "rating",
) -> str:
    """Build OpenTable search URL with query parameters."""
    if date_time is None:
        dt = datetime.now().replace(hour=19, minute=0, second=0, microsecond=0)
        date_time = dt.strftime("%Y-%m-%dT%H:%M:%S")

    params = {
        "dateTime": date_time,
        "covers": covers,
        "term": term,
        "intentModifiedTerm": term,
        "shouldUseLatLongSearch": "true",
        "showMap": "true",
        "metroId": metro_id,
        "sortBy": sort_by,
    }

    url = f"https://www.opentable.com/s?{urlencode(params)}"

    if region_ids:
        for rid in region_ids:
            url += f"&regionIds[]={rid}"

    if neighborhood_ids:
        for nid in neighborhood_ids:
            url += f"&neighborhoodIds[]={nid}"

    return url


async def search_restaurants(
    term: str = "",
    covers: int = 2,
    date_time: str | None = None,
    metro_id: int = 4,
    region_ids: list[int] | None = None,
    neighborhood_ids: list[int] | None = None,
    sort_by: str = "rating",
    limit: int = 10,
) -> list[dict]:
    """Search OpenTable for available reservations using browser automation."""
    # Lazy import: browser_use touches ~/.config on import, which fails in the
    # non-root sandbox tool-server and breaks tool loading.
    from browser_use import Agent, Browser, ChatBrowserUse

    url = build_search_url(
        term=term,
        covers=covers,
        date_time=date_time,
        metro_id=metro_id,
        region_ids=region_ids,
        neighborhood_ids=neighborhood_ids,
        sort_by=sort_by,
    )

    browser = Browser(
        cloud_profile_id="1da1d19b-4fa4-4f7a-bc2e-f3854df9db0b",
        cloud_proxy_country_code="us",
    )

    task = f"""
    1. Navigate to {url}
    2. Wait 5 seconds for page to fully load
    3. Extract restaurant information from the search results. For each restaurant card, extract:
       - Restaurant name
       - Cuisine type
       - Neighborhood/location
       - Available time slots (the clickable reservation times shown)
       - Rating if available
       - Price range if available
    4. Format the output as a JSON array with objects containing: name, cuisine, neighborhood, time_slots (array), rating, price_range
    5. Only extract the first {limit} restaurants
    6. Return ONLY the JSON array, nothing else
    """

    agent = Agent(
        task=task,
        browser=browser,
        llm=ChatBrowserUse(),
        max_steps=15,
    )

    try:
        history = await agent.run()
        result = history.final_result()

        if result:
            cleaned = result.replace("\\n", "\n").replace('\\"', '"')
            json_match = re.search(r"\[.*\]", cleaned, re.DOTALL)
            if json_match:
                json_str = json_match.group()
                try:
                    return json.loads(json_str)
                except json.JSONDecodeError:
                    pass
                try:
                    return ast.literal_eval(json_str)
                except (ValueError, SyntaxError):
                    pass
                try:
                    fixed = json_str.replace("'", '"')
                    return json.loads(fixed)
                except json.JSONDecodeError:
                    pass
        return []
    except Exception as e:
        raise RuntimeError(f"Failed to search OpenTable: {e}")


def search_sync(
    term: str = "",
    covers: int = 2,
    date_time: str | None = None,
    metro_id: int = 4,
    region_ids: list[int] | None = None,
    neighborhood_ids: list[int] | None = None,
    sort_by: str = "rating",
    limit: int = 10,
) -> list[dict]:
    """Synchronous wrapper for search_restaurants."""
    return asyncio.run(
        search_restaurants(
            term=term,
            covers=covers,
            date_time=date_time,
            metro_id=metro_id,
            region_ids=region_ids,
            neighborhood_ids=neighborhood_ids,
            sort_by=sort_by,
            limit=limit,
        )
    )
