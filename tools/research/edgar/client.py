"""Client for SEC EDGAR public data.

EDGAR needs no API key, but SEC rejects requests that lack a descriptive
User-Agent with contact details, and asks clients to stay at or below 10
requests per second. See https://www.sec.gov/os/accessing-edgar-data.

Everything except document bodies comes from SEC's JSON endpoints. Prefer the
XBRL JSON (`facts`, `frame`) for financial numbers; fetch and strip filing
HTML only when the narrative text is needed.

Endpoints:
- https://www.sec.gov/files/company_tickers.json (ticker -> CIK)
- https://data.sec.gov/submissions/CIK##########.json (filing history)
- https://data.sec.gov/api/xbrl/... (XBRL company facts, concepts, frames)
- https://efts.sec.gov/LATEST/search-index (full-text search, 2001+)
- https://www.sec.gov/Archives/edgar/data/... (filing documents)
"""

from __future__ import annotations

import os
import re
import time
import warnings
from datetime import date
from typing import Any

import httpx
from bs4 import BeautifulSoup, XMLParsedAsHTMLWarning

DEFAULT_USER_AGENT = "Centaur EDGAR tool admin@example.com"
USER_AGENT_ENV = "EDGAR_USER_AGENT"

WWW_URL = "https://www.sec.gov"
DATA_URL = "https://data.sec.gov"
SEARCH_URL = "https://efts.sec.gov/LATEST/search-index"
ARCHIVES_URL = f"{WWW_URL}/Archives/edgar/data"

# SEC allows 10 requests/second; stay a little under it.
MIN_REQUEST_INTERVAL_SECONDS = 0.12
RETRY_STATUSES = {429, 500, 502, 503, 504}
MAX_ATTEMPTS = 4
SEARCH_PAGE_SIZE = 100
DEFAULT_MAX_CHARS = 50_000

_ACCESSION_RE = re.compile(r"^(\d{10})-?(\d{2})-?(\d{6})$")


def _user_agent() -> str:
    """Return the SEC User-Agent, preferring the non-secret env override."""
    value = os.getenv(USER_AGENT_ENV, "").strip()  # noqa: TID251 - non-secret contact string
    return value or DEFAULT_USER_AGENT


def normalize_accession(accession: str) -> str:
    """Return an accession number in dashed form (0000320193-24-000123)."""
    match = _ACCESSION_RE.match(accession.strip())
    if not match:
        raise ValueError(f"Invalid accession number: {accession!r}")
    return "-".join(match.groups())


def _cik10(cik: int) -> str:
    return f"{cik:010d}"


def _archive_url(cik: int, accession: str, filename: str = "") -> str:
    folder = accession.replace("-", "")
    return f"{ARCHIVES_URL}/{cik}/{folder}/{filename}"


_SKIP_TAGS = ["script", "style", "head", "ix:header"]
_BLOCK_TAGS = ["p", "div", "br", "tr", "li", "table", "h1", "h2", "h3", "h4", "h5", "h6"]


def html_to_text(html: str) -> str:
    """Return readable text from an EDGAR HTML or inline-XBRL document."""
    with warnings.catch_warnings():
        # Inline XBRL filings are XHTML; the lenient HTML parser is what we want.
        warnings.simplefilter("ignore", XMLParsedAsHTMLWarning)
        soup = BeautifulSoup(html, "lxml")
    for tag in soup.find_all(_SKIP_TAGS):
        tag.decompose()
    for row in soup.find_all("tr"):
        cells = (cell.get_text(" ", strip=True) for cell in row.find_all(["td", "th"]))
        line = " | ".join(cell for cell in cells if cell)
        # Financial tables split "$", "%", and ")" into their own cells.
        line = re.sub(r"\$ \| ", "$", line)
        line = re.sub(r" \| (%|\))", r"\1", line)
        row.replace_with(f"\n{line}\n")
    for block in soup.find_all(_BLOCK_TAGS):
        block.insert_before("\n")
        block.insert_after("\n")
    raw = soup.get_text().replace("\xa0", " ")
    lines = (re.sub(r"[ \t\r\f\v]+", " ", line).strip(" |") for line in raw.split("\n"))
    return re.sub(r"\n{3,}", "\n\n", "\n".join(line.strip() for line in lines)).strip()


class EdgarClient:
    """Client for SEC EDGAR public APIs."""

    def __init__(self, user_agent: str | None = None, timeout: float = 30.0) -> None:
        self.user_agent = user_agent or _user_agent()
        self.timeout = timeout
        self._client: httpx.Client | None = None
        self._last_request = 0.0
        self._tickers: list[dict[str, Any]] | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout, follow_redirects=True)
        return self._client

    def _throttle(self) -> None:
        wait = MIN_REQUEST_INTERVAL_SECONDS - (time.monotonic() - self._last_request)
        if wait > 0:
            time.sleep(wait)
        self._last_request = time.monotonic()

    def _get(self, url: str, params: dict[str, Any] | None = None) -> httpx.Response:
        headers = {"User-Agent": self.user_agent, "Accept-Encoding": "gzip, deflate"}
        for attempt in range(MAX_ATTEMPTS):
            self._throttle()
            response = self.client.get(url, params=params, headers=headers)
            if response.status_code not in RETRY_STATUSES or attempt == MAX_ATTEMPTS - 1:
                break
            retry_after = response.headers.get("Retry-After", "")
            delay = float(retry_after) if retry_after.isdigit() else 2.0**attempt
            time.sleep(min(delay, 30.0))
        response.raise_for_status()
        return response

    def _get_json(self, url: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        result = self._get(url, params).json()
        if not isinstance(result, dict):
            raise RuntimeError("EDGAR returned an unexpected response.")
        return result

    # Companies

    def tickers(self) -> list[dict[str, Any]]:
        """Return SEC's ticker -> CIK mapping as [{cik, ticker, title}]."""
        if self._tickers is None:
            raw = self._get_json(f"{WWW_URL}/files/company_tickers.json")
            self._tickers = [
                {"cik": int(row["cik_str"]), "ticker": row["ticker"], "title": row["title"]}
                for row in raw.values()
                if isinstance(row, dict)
            ]
        return self._tickers

    def lookup(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        """Find companies by ticker, CIK, or name substring."""
        needle = query.strip().upper().replace(".", "-")
        rows = self.tickers()
        if needle.isdigit():
            return [row for row in rows if row["cik"] == int(needle)][:limit]
        exact = [row for row in rows if row["ticker"].upper() == needle]
        partial = [
            row
            for row in rows
            if needle in row["title"].upper().replace(".", "-") and row not in exact
        ]
        return (exact + partial)[:limit]

    def resolve_cik(self, identifier: str) -> int:
        """Resolve a ticker or numeric CIK to an integer CIK."""
        value = identifier.strip()
        if value.isdigit():
            return int(value)
        ticker = value.upper().replace(".", "-")
        for row in self.tickers():
            if row["ticker"].upper() == ticker:
                return row["cik"]
        raise LookupError(f"Unknown ticker {identifier!r}; try `edgar lookup` or pass a CIK.")

    # Filings

    def submissions(self, identifier: str) -> dict[str, Any]:
        """Return the raw submissions document for a company."""
        cik = self.resolve_cik(identifier)
        return self._get_json(f"{DATA_URL}/submissions/CIK{_cik10(cik)}.json")

    def filings(
        self,
        identifier: str,
        *,
        forms: list[str] | None = None,
        since: str | None = None,
        until: str | None = None,
        limit: int = 20,
    ) -> dict[str, Any]:
        """List a company's filings, newest first, following older pages as needed."""
        cik = self.resolve_cik(identifier)
        data = self._get_json(f"{DATA_URL}/submissions/CIK{_cik10(cik)}.json")
        wanted = {form.upper() for form in forms or []}
        results: list[dict[str, Any]] = []

        def collect(columns: dict[str, Any]) -> bool:
            """Append matching rows; return True when no further pages are needed."""
            for row in _rows(columns):
                filed = row.get("filingDate", "")
                if until and filed > until:
                    continue
                if since and filed < since:
                    return True
                if wanted and str(row.get("form", "")).upper() not in wanted:
                    continue
                results.append(_filing(cik, row))
                if len(results) >= limit:
                    return True
            return False

        filings = data.get("filings") or {}
        done = collect(filings.get("recent") or {})
        for page in filings.get("files") or []:
            if done:
                break
            older = self._get_json(f"{DATA_URL}/submissions/{page['name']}")
            done = collect(older)

        return {
            "cik": cik,
            "name": data.get("name"),
            "tickers": data.get("tickers", []),
            "exchanges": data.get("exchanges", []),
            "sic": data.get("sic"),
            "sic_description": data.get("sicDescription"),
            "fiscal_year_end": data.get("fiscalYearEnd"),
            "filings": results,
        }

    def search(
        self,
        query: str,
        *,
        forms: list[str] | None = None,
        start: str | None = None,
        end: str | None = None,
        identifier: str | None = None,
        limit: int = 20,
        offset: int = 0,
    ) -> dict[str, Any]:
        """Full-text search over EDGAR filings (2001 onward)."""
        params: dict[str, Any] = {"q": query}
        if forms:
            params["forms"] = ",".join(forms)
        if start or end:
            params["dateRange"] = "custom"
            params["startdt"] = start or "2001-01-01"
            params["enddt"] = end or date.today().isoformat()
        if identifier:
            params["ciks"] = _cik10(self.resolve_cik(identifier))

        results: list[dict[str, Any]] = []
        total: Any = None
        position = offset
        while len(results) < limit:
            params["from"] = position
            data = self._get_json(SEARCH_URL, params)
            hits = data.get("hits") or {}
            total = (hits.get("total") or {}).get("value", total)
            page = hits.get("hits") or []
            results.extend(_search_hit(hit) for hit in page)
            position += len(page)
            if len(page) < SEARCH_PAGE_SIZE:
                break
        return {"total": total, "offset": offset, "results": results[:limit]}

    def files(self, identifier: str, accession: str) -> dict[str, Any]:
        """List the documents in a filing."""
        cik = self.resolve_cik(identifier)
        accession = normalize_accession(accession)
        data = self._get_json(_archive_url(cik, accession, "index.json"))
        items = (data.get("directory") or {}).get("item") or []
        return {
            "cik": cik,
            "accession": accession,
            "files": [
                {
                    "name": item["name"],
                    "size": int(item["size"]) if str(item.get("size", "")).isdigit() else None,
                    "url": _archive_url(cik, accession, item["name"]),
                }
                for item in items
            ],
        }

    def document(
        self,
        identifier: str,
        accession: str,
        *,
        filename: str | None = None,
        max_chars: int = DEFAULT_MAX_CHARS,
        offset: int = 0,
        raw: bool = False,
    ) -> dict[str, Any]:
        """Fetch a filing document as text (the primary document by default)."""
        cik = self.resolve_cik(identifier)
        accession = normalize_accession(accession)
        if not filename:
            filename = self._primary_document(cik, accession)
        url = _archive_url(cik, accession, filename)
        body = self._get(url).text
        is_html = filename.lower().endswith((".htm", ".html", ".xml")) and not raw
        text = html_to_text(body) if is_html else body
        chunk = text[offset : offset + max_chars]
        return {
            "cik": cik,
            "accession": accession,
            "filename": filename,
            "url": url,
            "length": len(text),
            "offset": offset,
            "truncated": offset + len(chunk) < len(text),
            "text": chunk,
        }

    def _primary_document(self, cik: int, accession: str) -> str:
        data = self._get_json(f"{DATA_URL}/submissions/CIK{_cik10(cik)}.json")
        recent = (data.get("filings") or {}).get("recent") or {}
        for row in _rows(recent):
            if row.get("accessionNumber") == accession and row.get("primaryDocument"):
                return str(row["primaryDocument"])
        # Older filings are not in `recent`; fall back to the largest HTML file.
        listing = self.files(str(cik), accession)["files"]
        candidates = [
            item
            for item in listing
            if item["name"].lower().endswith((".htm", ".html"))
            and not item["name"].startswith(accession)
        ]
        if not candidates:
            candidates = [item for item in listing if item["name"].endswith(".txt")]
        if not candidates:
            raise LookupError(f"No document found in filing {accession}.")
        return max(candidates, key=lambda item: item["size"] or 0)["name"]

    # XBRL financial data

    def concepts(self, identifier: str, taxonomy: str | None = None) -> dict[str, Any]:
        """List the XBRL concepts a company reports."""
        cik = self.resolve_cik(identifier)
        data = self._get_json(f"{DATA_URL}/api/xbrl/companyfacts/CIK{_cik10(cik)}.json")
        concepts = []
        for tax, tags in (data.get("facts") or {}).items():
            if taxonomy and tax != taxonomy:
                continue
            for tag, body in tags.items():
                units = body.get("units") or {}
                concepts.append(
                    {
                        "concept": f"{tax}:{tag}",
                        "label": body.get("label"),
                        "units": sorted(units),
                        "facts": sum(len(values) for values in units.values()),
                    }
                )
        return {"cik": cik, "entity": data.get("entityName"), "concepts": concepts}

    def facts(
        self,
        identifier: str,
        concept: str,
        *,
        form: str | None = None,
        unit: str | None = None,
        limit: int = 20,
    ) -> dict[str, Any]:
        """Return the latest reported values for one XBRL concept."""
        cik = self.resolve_cik(identifier)
        taxonomy, tag = _split_concept(concept)
        data = self._get_json(
            f"{DATA_URL}/api/xbrl/companyconcept/CIK{_cik10(cik)}/{taxonomy}/{tag}.json"
        )
        units: dict[str, list[dict[str, Any]]] = {}
        for unit_name, values in (data.get("units") or {}).items():
            if unit and unit_name != unit:
                continue
            rows = [v for v in values if not form or str(v.get("form", "")).upper() == form.upper()]
            rows.sort(key=lambda v: (v.get("end", ""), v.get("filed", "")), reverse=True)
            units[unit_name] = rows[:limit]
        return {
            "cik": cik,
            "entity": data.get("entityName"),
            "concept": f"{taxonomy}:{tag}",
            "label": data.get("label"),
            "description": data.get("description"),
            "units": units,
        }

    def frame(
        self,
        concept: str,
        period: str,
        *,
        unit: str = "USD",
        limit: int = 20,
        ascending: bool = False,
    ) -> dict[str, Any]:
        """Return one concept across all companies for a period (e.g. CY2023, CY2023Q4I)."""
        taxonomy, tag = _split_concept(concept)
        data = self._get_json(f"{DATA_URL}/api/xbrl/frames/{taxonomy}/{tag}/{unit}/{period}.json")
        rows = sorted(data.get("data") or [], key=lambda r: r.get("val", 0), reverse=not ascending)
        return {
            "concept": f"{taxonomy}:{tag}",
            "label": data.get("label"),
            "unit": unit,
            "period": period,
            "companies": len(data.get("data") or []),
            "data": rows[:limit],
        }

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> EdgarClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _rows(columns: dict[str, Any]) -> list[dict[str, Any]]:
    """Turn EDGAR's columnar filing arrays into row dicts."""
    keys = [key for key, value in columns.items() if isinstance(value, list)]
    count = len(columns.get("accessionNumber") or [])
    return [{key: columns[key][i] for key in keys if i < len(columns[key])} for i in range(count)]


def _filing(cik: int, row: dict[str, Any]) -> dict[str, Any]:
    accession = row.get("accessionNumber", "")
    primary = row.get("primaryDocument") or ""
    return {
        "accession": accession,
        "form": row.get("form"),
        "filed": row.get("filingDate"),
        "report_date": row.get("reportDate") or None,
        "description": row.get("primaryDocDescription") or None,
        "items": row.get("items") or None,
        "primary_document": primary or None,
        "url": _archive_url(cik, accession, primary) if accession else None,
    }


def _search_hit(hit: dict[str, Any]) -> dict[str, Any]:
    source = hit.get("_source") or {}
    accession = source.get("adsh") or str(hit.get("_id", "")).split(":", 1)[0]
    filename = str(hit.get("_id", "")).split(":", 1)[-1]
    ciks = source.get("ciks") or []
    url = _archive_url(int(ciks[0]), accession, filename) if ciks and accession else None
    return {
        "accession": accession,
        "filename": filename,
        "form": source.get("form"),
        "filed": source.get("file_date"),
        "period_ending": source.get("period_ending"),
        "companies": source.get("display_names") or [],
        "ciks": ciks,
        "url": url,
    }


def _split_concept(concept: str) -> tuple[str, str]:
    taxonomy, _, tag = concept.rpartition(":")
    if not tag:
        raise ValueError(f"Invalid XBRL concept: {concept!r}")
    return taxonomy or "us-gaap", tag


def _client() -> EdgarClient:
    return EdgarClient()
