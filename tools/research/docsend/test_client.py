import asyncio
import base64
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
_rendered_pdf_filename = _CLIENT._rendered_pdf_filename


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


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("Investor Deck.pptx", "Investor Deck.pdf"),
        ("Quarterly Update", "Quarterly Update.pdf"),
        ("archive.tar.gz", "archive.tar.pdf"),
        ("\x00", "docsend_document.pdf"),
    ],
)
def test_rendered_pdf_filename_replaces_the_source_extension(name: str, expected: str) -> None:
    assert _rendered_pdf_filename(name) == expected


def test_recover_rendered_document_builds_a_pdf_from_visible_pages(monkeypatch) -> None:
    calls: list[tuple[str, object]] = []

    class FakeImage:
        def save(self, buffer, file_format, *, save_all, append_images) -> None:
            calls.append(("save", (file_format, save_all, len(append_images))))
            buffer.write(b"rendered-pdf")

    async def slide_count(page) -> int:
        return 2

    async def navigate(page, total: int) -> None:
        calls.append(("navigate", total))

    async def extract(page) -> list[str]:
        return ["https://example.test/1", "https://example.test/2"]

    async def download(urls: list[str]) -> list[FakeImage]:
        calls.append(("download", urls))
        return [FakeImage(), FakeImage()]

    monkeypatch.setattr(_CLIENT, "_slide_count", slide_count)
    monkeypatch.setattr(_CLIENT, "_navigate_all_slides", navigate)
    monkeypatch.setattr(_CLIENT, "_extract_dom_image_urls", extract)
    monkeypatch.setattr(_CLIENT, "_download_images", download)

    result = asyncio.run(_CLIENT._recover_rendered_document(object(), filename="Investor Deck.pdf"))

    assert result["status"] == "ok"
    assert result["filename"] == "Investor Deck.pdf"
    assert result["page_count"] == 2
    assert base64.b64decode(result["data"]) == b"rendered-pdf"
    assert calls == [
        ("navigate", 2),
        ("download", ["https://example.test/1", "https://example.test/2"]),
        ("save", ("PDF", True, 1)),
    ]


def test_fetch_space_item_recovers_a_document_when_download_is_disabled(monkeypatch) -> None:
    class DisabledDownloadButton:
        async def count(self) -> int:
            return 0

        async def is_enabled(self) -> bool:
            raise AssertionError("a missing button must not be queried for enabled state")

    class Row:
        def locator(self, selector: str):
            assert selector == 'button[aria-label="Download file"]'
            return DisabledDownloadButton()

    item = _CLIENT._make_space_item(
        name="Investor Deck",
        path="Investor Deck",
        item_type="file",
        downloadable=False,
        download_method="rendered_pdf",
    )

    async def find_row(page, selected_item):
        assert selected_item == item
        return Row(), None

    async def recover(page, selected_item, row):
        assert selected_item == item
        return {
            "status": "ok",
            "filename": "Investor Deck.pdf",
            "download_method": "rendered_pdf",
            "data": "cGRm",
            "error": None,
        }

    async def unexpected_records(*args, **kwargs):
        raise AssertionError("Browserbase downloads must not be queried for rendered recovery")

    monkeypatch.setattr(_CLIENT, "_find_space_file_row", find_row)
    monkeypatch.setattr(_CLIENT, "_recover_space_document", recover)
    monkeypatch.setattr(_CLIENT, "_browserbase_download_records", unexpected_records)

    result = asyncio.run(
        _CLIENT._fetch_space_item(object(), object(), "api-key", "session-id", item)
    )

    assert result["status"] == "ok"
    assert result["download_method"] == "rendered_pdf"
    assert result["item_id"] == item["id"]


def test_fetch_space_item_uses_original_download_when_enabled(monkeypatch) -> None:
    calls: list[str] = []

    class EnabledDownloadButton:
        async def count(self) -> int:
            return 1

        async def is_enabled(self) -> bool:
            return True

    class Row:
        def locator(self, selector: str):
            assert selector == 'button[aria-label="Download file"]'
            return EnabledDownloadButton()

    item = _CLIENT._make_space_item(
        name="Financials.xlsx",
        path="Financials.xlsx",
        item_type="file",
        downloadable=True,
        download_method="original",
    )

    async def find_row(page, selected_item):
        return Row(), None

    async def records(api_key: str, session_id: str) -> list[dict]:
        calls.append("records")
        return [{"id": "existing"}]

    async def configure(browser) -> None:
        calls.append("configure")

    async def click(page, selected_item, row) -> dict:
        calls.append("click")
        return {"status": "ok", "error": None}

    async def wait(api_key: str, session_id: str, existing_ids: set[str]):
        calls.append("wait")
        assert existing_ids == {"existing"}
        return "Financials.xlsx", b"original-file"

    async def unexpected_recovery(*args, **kwargs):
        raise AssertionError("rendered recovery must not run when original download is enabled")

    monkeypatch.setattr(_CLIENT, "_find_space_file_row", find_row)
    monkeypatch.setattr(_CLIENT, "_browserbase_download_records", records)
    monkeypatch.setattr(_CLIENT, "_configure_browserbase_downloads", configure)
    monkeypatch.setattr(_CLIENT, "_click_space_file_download", click)
    monkeypatch.setattr(_CLIENT, "_wait_for_new_download", wait)
    monkeypatch.setattr(_CLIENT, "_recover_space_document", unexpected_recovery)

    result = asyncio.run(
        _CLIENT._fetch_space_item(object(), object(), "api-key", "session-id", item)
    )

    assert result["status"] == "ok"
    assert result["filename"] == "Financials.xlsx"
    assert result["download_method"] == "original"
    assert base64.b64decode(result["data"]) == b"original-file"
    assert calls == ["records", "configure", "click", "wait"]


def test_recover_space_document_opens_viewer_in_authenticated_context(monkeypatch) -> None:
    calls: list[object] = []

    class Link:
        @property
        def first(self):
            return self

        async def count(self) -> int:
            return 1

        async def get_attribute(self, name: str) -> str:
            assert name == "href"
            return "/view/deck123/d/room-document"

    class Row:
        def locator(self, selector: str):
            assert selector == "a[href]"
            return Link()

    class DocumentPage:
        async def goto(self, url: str, **kwargs) -> None:
            calls.append(("goto", url))

        async def close(self) -> None:
            calls.append("close")

    document_page = DocumentPage()

    class Context:
        async def new_page(self):
            calls.append("new_page")
            return document_page

    class SpacePage:
        url = "https://docsend.com/view/s/space123"
        context = Context()

    item = _CLIENT._make_space_item(
        name="Investor Deck",
        path="Investor Deck",
        item_type="file",
        downloadable=False,
        download_method="rendered_pdf",
    )

    async def detect_state(page) -> str:
        return "ready"

    async def has_verification_wall(page) -> bool:
        return False

    async def dismiss_cookies(page) -> None:
        calls.append("dismiss")

    async def recover(page, *, filename: str) -> dict:
        calls.append(("recover", filename))
        return {"status": "ok", "filename": filename, "data": "cGRm", "error": None}

    monkeypatch.setattr(_CLIENT, "_detect_state", detect_state)
    monkeypatch.setattr(_CLIENT, "_has_verification_wall", has_verification_wall)
    monkeypatch.setattr(_CLIENT, "_dismiss_cookies", dismiss_cookies)
    monkeypatch.setattr(_CLIENT, "_recover_rendered_document", recover)

    result = asyncio.run(_CLIENT._recover_space_document(SpacePage(), item, Row()))

    assert result["status"] == "ok"
    assert result["download_method"] == "rendered_pdf"
    assert calls == [
        "new_page",
        ("goto", "https://docsend.com/view/deck123/d/room-document"),
        "dismiss",
        ("recover", "Investor Deck.pdf"),
        "close",
    ]
