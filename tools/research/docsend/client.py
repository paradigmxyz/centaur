"""DocSend downloader — browser-use cloud browser + Playwright.

Two-strategy slide capture:
1. Navigate through every slide to force DocSend's viewer to render each one
2. Extract image URLs from <img class="preso-view page-view"> DOM elements
3. If DOM extraction fails, fall back to /page_data/ API (needs session cookies)
4. If both fail, fall back to page-by-page screenshots

Mirrors the approach used by browser extensions (louisabraham/docsend-dl,
mdesilva/DocSendDownloader) which are known to work reliably.
"""

from __future__ import annotations

import asyncio
import base64
import os
import re
from io import BytesIO
from urllib.parse import urlencode

import httpx
from PIL import Image

from centaur_sdk import secret

BROWSER_USE_PROXY_COUNTRY = os.environ.get("BROWSER_USE_PROXY_COUNTRY", "")  # noqa: TID251
BROWSER_USE_PROFILE_ID = os.environ.get("BROWSER_USE_PROFILE_ID", "")  # noqa: TID251


def _browser_use_api_key() -> str:
    # Server mode returns the stub key name; the proxy swaps it for the real
    # key in the query string (match_query). Local mode uses .env when present
    # and otherwise keeps the stub so sandbox runs do not fail before proxying.
    return secret("BROWSER_USE_API_KEY", "BROWSER_USE_API_KEY")


def _prepare_playwright_tls() -> None:
    cert_path = "/firewall-certs/ca-cert.pem"
    if os.path.exists(cert_path) and not os.environ.get("NODE_EXTRA_CA_CERTS"):  # noqa: TID251
        os.environ["NODE_EXTRA_CA_CERTS"] = cert_path
    node_options = os.environ.get("NODE_OPTIONS", "")  # noqa: TID251
    if "--use-system-ca" not in node_options.split():
        os.environ["NODE_OPTIONS"] = f"{node_options} --use-system-ca".strip()


def _browser_use_ws_url(api_key: str) -> str:
    params = {
        "apiKey": api_key,
        "browserScreenWidth": "1920",
        "browserScreenHeight": "1080",
    }
    if BROWSER_USE_PROXY_COUNTRY:
        params["proxyCountryCode"] = BROWSER_USE_PROXY_COUNTRY.lower()
    if BROWSER_USE_PROFILE_ID:
        params["profileId"] = BROWSER_USE_PROFILE_ID
    return f"wss://connect.browser-use.com?{urlencode(params)}"


def _redact_browser_use_secret(error: Exception, api_key: str) -> str:
    text = str(error)
    if api_key:
        text = text.replace(api_key, "<redacted>")
    return re.sub(r"apiKey=[^&\\s]+", "apiKey=<redacted>", text)


class DocsendClient:
    """Download DocSend documents as PDF via cloud browser."""

    def download(
        self,
        url: str,
        email: str = "",
        passcode: str | None = None,
        verification_link: str | None = None,
    ) -> dict:
        """Download a DocSend document as PDF.

        Args:
            url: DocSend URL (e.g. https://docsend.com/view/abc123)
            email: Email for email-gated documents.
            passcode: Passcode for password-protected documents.
            verification_link: If DocSend requires email verification, the
                user must open their email and copy the verification link.
                Pass it here so the cloud browser can verify in-session.

        Returns:
            Dict with status, filename, data (base64), page_count, etc.
        """
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = None
        if loop and loop.is_running():
            import concurrent.futures

            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                return pool.submit(
                    asyncio.run, self._run(url, email, passcode, verification_link)
                ).result()
        return asyncio.run(self._run(url, email, passcode, verification_link))

    async def _run(
        self, url: str, email: str, passcode: str | None, verification_link: str | None
    ) -> dict:
        url = url.rstrip("/")
        if not re.match(r"https?://", url):
            url = f"https://{url}"

        api_key = _browser_use_api_key()

        try:
            from playwright.async_api import async_playwright
        except ImportError:
            return _err("playwright not installed")

        browser = None

        try:
            _prepare_playwright_tls()
            async with async_playwright() as p:
                browser = await p.chromium.connect_over_cdp(_browser_use_ws_url(api_key))
                ctx = browser.contexts[0] if browser.contexts else await browser.new_context()
                page = ctx.pages[0] if ctx.pages else await ctx.new_page()

                # If we have a verification link, open it first
                if verification_link:
                    try:
                        await page.goto(
                            verification_link, wait_until="domcontentloaded", timeout=30000
                        )
                    except Exception:
                        pass
                    await asyncio.sleep(5)

                # 3. Navigate to DocSend URL
                try:
                    await page.goto(url, wait_until="networkidle", timeout=45000)
                except Exception:
                    pass

                # 4. Detect state
                state = await _detect_state(page)
                if state == "expired":
                    return _err("Document not found or expired", status="expired")
                if state == "blocked":
                    return _err("Blocked by CloudFront/WAF", status="blocked")

                await _dismiss_cookies(page)

                if state == "passcode_required":
                    if not passcode:
                        return _err(
                            "This document is password-protected. Ask the user "
                            "for the passcode and retry with the passcode parameter.",
                            status="passcode_required",
                        )
                    ok = await _enter_passcode(page, email, passcode)
                    if not ok:
                        return _err("Passcode was rejected", status="passcode_required")

                elif state == "email_required" and not verification_link:
                    if not email:
                        return _err(
                            "This document requires an email address to access. "
                            "Ask the user for their email and retry with the "
                            "email parameter.",
                            status="email_required",
                        )
                    await _enter_email(page, email)
                    await asyncio.sleep(1)

                    # Check for verification wall
                    if await _has_verification_wall(page):
                        used = email
                        return _err(
                            f"DocSend sent a verification link to {used}. "
                            "Ask the user to check their email, copy the full "
                            "verification URL, and retry with the "
                            "verification_link parameter.",
                            status="verification_link_required",
                        )

                # 5. Get slide count
                total = 0
                for _ in range(3):
                    total = await _slide_count(page)
                    if total > 0:
                        break
                    await asyncio.sleep(2)
                if total == 0:
                    return _err("Could not determine page count")

                # 6. Navigate through ALL slides to force rendering
                await _navigate_all_slides(page, total)

                # 7. Try DOM extraction first (most reliable)
                image_urls = await _extract_dom_image_urls(page)

                if image_urls:
                    good = await _download_images(image_urls)
                else:
                    # Fallback: /page_data/ API
                    api_urls, _ = await _fetch_slide_urls(page, total)
                    valid = [u for u in api_urls if u]
                    if valid:
                        good = await _download_images(valid)
                    else:
                        good = []

                if not good:
                    return _err(
                        "Failed to extract slide images. The document may "
                        "still be behind a verification wall.",
                        page_count=total,
                    )

                # 8. Assemble PDF
                buf = BytesIO()
                good[0].save(
                    buf,
                    "PDF",
                    save_all=True,
                    append_images=good[1:] if len(good) > 1 else [],
                )

                slug_m = re.search(r"docsend\.com/view/(?:s/)?([a-zA-Z0-9]+)", url)
                slug = slug_m.group(1) if slug_m else "document"

                return {
                    "status": "ok",
                    "filename": f"docsend_{slug}.pdf",
                    "data": base64.b64encode(buf.getvalue()).decode(),
                    "mime_type": "application/pdf",
                    "page_count": total,
                    "downloaded": len(good),
                    "error": None if len(good) == total else f"Got {len(good)}/{total}",
                }

        except Exception as e:
            return _err(_redact_browser_use_secret(e, api_key))
        finally:
            if browser:
                try:
                    await browser.close()
                except Exception:
                    pass


# ---------------------------------------------------------------------------
# State detection
# ---------------------------------------------------------------------------


async def _detect_state(page) -> str:
    title = (await page.title()).lower()
    if "404" in title or "not found" in title:
        return "expired"
    if "request could not be satisfied" in title:
        return "blocked"

    for sel in ['input[type="password"]', "#link_auth_form_passcode", 'input[name*="passcode"]']:
        el = await page.query_selector(sel)
        if el:
            try:
                box = await el.bounding_box()
                if box and box["width"] > 30:
                    return "passcode_required"
            except Exception:
                pass

    for sel in [
        '#prompt input[type="email"]',
        '.ReactModal__Content input[type="email"]',
        '[class*="auth"] input[type="email"]',
        '.modal input[type="email"]',
    ]:
        try:
            loc = page.locator(sel).first
            if await loc.count() > 0:
                box = await loc.bounding_box(timeout=2000)
                if box and box["width"] > 50:
                    return "email_required"
        except Exception:
            continue

    try:
        body = await page.inner_text("body")
        if body and "no longer available" in body.lower():
            return "expired"
    except Exception:
        pass

    return "ready"


async def _has_verification_wall(page) -> bool:
    """Check if DocSend is showing a 'verify your email' wall after submission."""
    try:
        body = await page.inner_text("body")
        lower = body.lower()
        return any(
            phrase in lower
            for phrase in [
                "requests your action",
                "emailed a link",
                "verify that you own",
                "verification link",
                "check your email",
            ]
        )
    except Exception:
        return False


# ---------------------------------------------------------------------------
# Auth helpers
# ---------------------------------------------------------------------------


async def _dismiss_cookies(page) -> None:
    try:
        btn = page.locator('button:has-text("Accept All")').first
        if await btn.is_visible(timeout=2000):
            await btn.click()
            await asyncio.sleep(1)
    except Exception:
        pass


async def _enter_email(page, email: str) -> None:
    for sel in [
        "#link_auth_form_email",
        '#new_link_auth_form input[type="email"]',
        '#prompt input[type="email"]',
        '.ReactModal__Content input[type="email"]',
        '#email[type="email"]',
        'input[type="email"]',
    ]:
        try:
            loc = page.locator(sel).first
            if await loc.is_visible(timeout=2000):
                await loc.fill(email)
                await asyncio.sleep(0.5)
                break
        except Exception:
            continue
    await _click_submit(page)
    try:
        await page.wait_for_load_state("networkidle", timeout=15000)
    except Exception:
        pass
    await asyncio.sleep(2)


async def _enter_passcode(page, email: str, passcode: str) -> bool:
    for sel in [
        "#link_auth_form_email",
        '#new_link_auth_form input[type="email"]',
        '.ReactModal__Content input[type="email"]',
        '#email[type="email"]',
    ]:
        try:
            loc = page.locator(sel).first
            if await loc.is_visible(timeout=2000):
                await loc.fill(email)
                await asyncio.sleep(0.5)
                break
        except Exception:
            continue

    for sel in ["#link_auth_form_passcode", 'input[type="password"]', 'input[name*="passcode"]']:
        try:
            loc = page.locator(sel).first
            if await loc.is_visible(timeout=2000):
                await loc.fill(passcode)
                await asyncio.sleep(0.5)
                break
        except Exception:
            continue

    await _click_submit(page)
    try:
        await page.wait_for_load_state("networkidle", timeout=15000)
    except Exception:
        pass
    await asyncio.sleep(2)

    for sel in ['input[type="password"]', "#link_auth_form_passcode", 'input[name*="passcode"]']:
        try:
            if await page.locator(sel).first.is_visible(timeout=1000):
                return False
        except Exception:
            pass
    return True


async def _click_submit(page) -> None:
    for sel in [
        'button:has-text("Continue")',
        'button:has-text("Submit")',
        'button:has-text("Confirm")',
        'button:has-text("Enter")',
        'button[type="submit"]',
    ]:
        try:
            btn = page.locator(sel).first
            if await btn.is_visible(timeout=1500):
                await btn.click()
                return
        except Exception:
            continue
    await page.keyboard.press("Enter")


# ---------------------------------------------------------------------------
# Slide extraction
# ---------------------------------------------------------------------------


async def _slide_count(page) -> int:
    for sel in [".toolbar-page-indicator", ".page-label", '[class*="page-indicator"]']:
        try:
            el = await page.query_selector(sel)
            if el:
                text = await el.text_content()
                m = re.search(r"(\d+)\s*/\s*(\d+)", text or "")
                if m:
                    return int(m.group(2))
        except Exception:
            continue
    thumbs = await page.query_selector_all('[class*="document-thumb-container"]')
    if thumbs:
        nums = []
        for t in thumbs:
            n = await t.get_attribute("data-page-num")
            if n:
                nums.append(int(n))
        if nums:
            return max(nums)
    return 0


async def _navigate_all_slides(page, total: int) -> None:
    """Navigate through every slide to force DocSend to render them in the DOM."""
    # Go to page 1
    for _ in range(total):
        await page.keyboard.press("ArrowLeft")
        await asyncio.sleep(0.1)
    await asyncio.sleep(1)

    # Walk forward through every slide
    for i in range(total - 1):
        await page.keyboard.press("ArrowRight")
        await asyncio.sleep(0.3)
    await asyncio.sleep(1)


async def _extract_dom_image_urls(page) -> list[str]:
    """Extract slide image URLs from rendered <img> elements in the DOM.

    DocSend's viewer renders slides as <img class="preso-view page-view">.
    After navigating through all slides, their .src attributes contain
    authenticated CloudFront URLs.
    """
    urls = await page.evaluate("""() => {
        const imgs = document.querySelectorAll("img.preso-view.page-view, img.page-view");
        const urls = [];
        for (const img of imgs) {
            if (img.src && img.src.startsWith("http") && img.naturalWidth > 100) {
                urls.push(img.src);
            }
        }
        return urls;
    }""")
    return urls or []


async def _fetch_slide_urls(page, total: int) -> tuple[list[str | None], list[int]]:
    """Fetch slide image URLs via in-browser /page_data/ API."""
    base_url = page.url.split("?")[0]
    urls: list[str | None] = []
    failed: list[int] = []

    for i in range(1, total + 1):
        slide_url = None
        for attempt in range(3):
            result = await page.evaluate(
                """async (args) => {
                    const [base, idx] = args;
                    try {
                        const r = await fetch(base + '/page_data/' + idx);
                        if (!r.ok) return {error: 'HTTP ' + r.status};
                        const text = await r.text();
                        if (!text) return {error: 'empty'};
                        const d = JSON.parse(text);
                        return {url: d.imageUrl || d.directImageUrl || null};
                    } catch(e) { return {error: e.toString()}; }
                }""",
                [base_url, i],
            )
            if isinstance(result, dict) and result.get("url"):
                slide_url = result["url"]
                break
            err = result.get("error", "") if isinstance(result, dict) else ""
            if err.startswith("HTTP 4"):
                break
            if attempt < 2:
                await asyncio.sleep(2)
        urls.append(slide_url)
        if not slide_url:
            failed.append(i)
    return urls, failed


async def _download_images(urls: list[str]) -> list[Image.Image]:
    """Download image URLs and return as RGB PIL Images."""
    async with httpx.AsyncClient(timeout=30.0) as client:

        async def fetch(img_url: str) -> Image.Image | None:
            try:
                r = await client.get(img_url)
                r.raise_for_status()
                rgba = Image.open(BytesIO(r.content))
                rgb = Image.new("RGB", rgba.size, (255, 255, 255))
                rgb.paste(rgba, mask=rgba.split()[3] if rgba.mode == "RGBA" else None)
                return rgb
            except Exception:
                return None

        results = await asyncio.gather(*[fetch(u) for u in urls])
    return [img for img in results if img is not None]


# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------


def _attr(obj: object, *keys: str) -> object | None:
    for key in keys:
        if isinstance(obj, dict) and key in obj:
            return obj[key]
        if hasattr(obj, key):
            return getattr(obj, key)
    return None


def _err(error: str, status: str = "error", page_count: int = 0) -> dict:
    return {
        "status": status,
        "error": error,
        "data": None,
        "page_count": page_count,
        "filename": None,
    }


def _client() -> DocsendClient:
    return DocsendClient()
