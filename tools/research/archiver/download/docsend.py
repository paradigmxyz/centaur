#!/usr/bin/env python3
"""
DocSend Router - Intelligent routing for DocSend URLs.

This module provides:
- State detection for DocSend URLs (expired, password required, download enabled, etc.)
- Routing logic to handle each state appropriately
- Orchestration for processing multiple URLs
"""

import asyncio
import os
import re
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from urllib.parse import urlparse

from browser_use_sdk import AsyncBrowserUse
from playwright.async_api import Page, async_playwright
from playwright.async_api import TimeoutError as PlaywrightTimeout


# Import slide scraper functions
from ..docsend.playwright import (
    DEFAULT_CONFIG,
    DEFAULT_EMAIL,
    ScrapeConfig,
    ScrapeResult,
    ScrapeStatus,
    browser_use_api_key,
    browser_use_ws_url,
    create_pdf,
    download_images,
    enter_password,
    fetch_slide_urls,
    get_slide_count,
    prepare_playwright_tls,
    redact_browser_use_secret,
)

# Configuration
BROWSER_USE_PROXY_COUNTRY = os.environ.get("BROWSER_USE_PROXY_COUNTRY", "")  # noqa: TID251
BROWSER_USE_PROFILE_ID = os.environ.get("BROWSER_USE_PROFILE_ID", "")  # noqa: TID251
DOCSEND_DEBUG_DIR = os.environ.get("DOCSEND_DEBUG_DIR")  # noqa: TID251
MAX_PARALLEL = 5


def _session_value(session: object, *keys: str) -> object | None:
    for key in keys:
        if isinstance(session, dict) and key in session:
            return session[key]
        if hasattr(session, key):
            return getattr(session, key)
    return None


async def _create_browser_session(client: AsyncBrowserUse, **kwargs):
    browsers = client.browsers
    if hasattr(browsers, "create_browser_session"):
        return await browsers.create_browser_session(**kwargs)
    return await browsers.create(**kwargs)


async def _stop_browser_session(client: AsyncBrowserUse, session_id: str) -> None:
    browsers = client.browsers
    if hasattr(browsers, "update_browser_session"):
        await browsers.update_browser_session(session_id=session_id, action="stop")
        return
    if hasattr(browsers, "stop"):
        await browsers.stop(session_id)
        return
    await browsers.update(session_id, action="stop")


class DocSendState(Enum):
    """Possible states of a DocSend link."""

    LINK_EXPIRED = "link_expired"  # 404 or "not found"
    PASSWORD_REQUIRED = "password_required"  # Password field visible
    EMAIL_GATED = "email_gated"  # Email required to view
    DOWNLOAD_ENABLED = "download_enabled"  # Download button available
    DOWNLOAD_DISABLED = "download_disabled"  # View-only, need to scrape slides
    FOLDER = "folder"  # Folder/space with ZIP download
    BLOCKED = "blocked"  # CloudFront or WAF block
    UNKNOWN = "unknown"  # Couldn't determine


def is_folder_url(url: str) -> bool:
    """Check if a DocSend URL is a folder/space link (contains /s/ segment)."""
    path = urlparse(url).path
    # Folder URLs: /view/s/xxx or /v/xxx/s/yyy
    return "/s/" in path


@dataclass
class StateDetectionResult:
    """Result of state detection."""

    state: DocSendState
    title: str | None = None
    page_count: int | None = None
    error: str | None = None


async def enter_email(page: Page, email: str, config: ScrapeConfig = DEFAULT_CONFIG) -> bool:
    """Enter email on an email-gated DocSend page.

    Returns:
        True if email was accepted, False otherwise.
    """
    try:
        # Try multiple selectors for email field
        email_selectors = [
            '#prompt input[type="email"]',
            '.ReactModal__Content input[type="email"]',
            '#email[type="email"]',
            '.modal input[type="email"]',
            '[class*="auth"] input[type="email"]',
            'input[type="email"]',
        ]

        email_field = None
        for selector in email_selectors:
            try:
                field = page.locator(selector).first
                if await field.count() > 0:
                    box = await field.bounding_box(timeout=2000)
                    if box and box["width"] > 50:
                        email_field = field
                        break
            except Exception:
                continue

        if not email_field:
            return False

        await email_field.click(timeout=5000)
        await email_field.fill(email, timeout=5000)

        # Try to click continue/confirm button
        submit_selectors = [
            'button:has-text("Continue")',
            'button:has-text("Confirm")',
            'button[type="submit"]',
        ]
        clicked = False
        for sel in submit_selectors:
            try:
                btn = page.locator(sel).first
                if await btn.is_visible(timeout=1500):
                    await btn.click(timeout=5000)
                    clicked = True
                    break
            except Exception:
                continue
        if not clicked:
            await page.keyboard.press("Enter")

        # Wait for document to load
        await page.wait_for_load_state("networkidle", timeout=config.network_idle_timeout)
        await asyncio.sleep(2)

        return True

    except PlaywrightTimeout:
        return False
    except Exception:
        return False


async def detect_docsend_state(
    page: Page,
    url: str,
    password: str | None = None,
    http_status: int | None = None,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> StateDetectionResult:
    """Detect the current state of a DocSend page.

    Args:
        page: Playwright page already navigated to the DocSend URL
        url: The DocSend URL (for logging)
        password: Optional password to try if page is password-protected
        config: Scraping configuration

    Returns:
        StateDetectionResult with detected state and metadata
    """
    result = StateDetectionResult(state=DocSendState.UNKNOWN)

    try:
        # Detect CloudFront/WAF blocks before treating as expired
        title = await page.title()
        result.title = title

        if http_status in (403, 429, 503):
            result.state = DocSendState.BLOCKED
            result.error = f"http_{http_status}"
            return result

        if "request could not be satisfied" in title.lower():
            result.state = DocSendState.BLOCKED
            result.error = "cloudfront_blocked"
            return result

        try:
            blocked_text = await page.locator(
                "text=/request could not be satisfied|cloudfront|access denied|request blocked/i"
            ).first.text_content(timeout=2000)
            if blocked_text:
                result.state = DocSendState.BLOCKED
                result.error = "cloudfront_blocked"
                return result
        except Exception:
            pass

        if "404" in title or "not found" in title.lower() or "expired" in title.lower():
            result.state = DocSendState.LINK_EXPIRED
            return result

        # Check for password/passcode field BEFORE body-text expired checks,
        # because email/password-gated pages can show "no longer available"
        # as placeholder text before auth.
        # DocSend uses type="text" for passcode fields, not type="password".
        password_field = await page.query_selector(
            'input[type="password"], #link_auth_form_passcode, input[name*="passcode"]'
        )
        if not password_field:
            # Fallback: try selectors individually (some CDP connections
            # have issues with comma-separated selectors)
            for pw_sel in [
                'input[type="password"]',
                "#link_auth_form_passcode",
                'input[name*="passcode"]',
            ]:
                password_field = await page.query_selector(pw_sel)
                if password_field:
                    break
        if password_field:
            if password:
                # Try the password (enter_password also fills email if present)
                success = await enter_password(page, password, config)
                if not success:
                    result.state = DocSendState.PASSWORD_REQUIRED
                    result.error = "Password rejected or entry failed"
                    return result
                # Password worked — skip email/expired checks, go straight
                # to download button and slide count detection below.
            else:
                result.state = DocSendState.PASSWORD_REQUIRED
                return result
        else:
            # No password field — check email gate and expired text
            has_email_field = False
            try:
                email_locator = page.locator('#prompt input[type="email"]').first
                await email_locator.wait_for(state="visible", timeout=config.auth_timeout)
                has_email_field = True
            except Exception:
                # Try fallback selectors (including folder-style ReactModal email)
                fallback_selectors = [
                    '.modal input[type="email"], [class*="auth"] input[type="email"]',
                    '.ReactModal__Content input[type="email"], #email[type="email"]',
                ]
                for fb_sel in fallback_selectors:
                    if has_email_field:
                        break
                    try:
                        modal_email = page.locator(fb_sel).first
                        if await modal_email.count() > 0:
                            box = await modal_email.bounding_box(timeout=2000)
                            if box and box["width"] > 50:
                                has_email_field = True
                    except Exception:
                        pass

            if has_email_field:
                result.state = DocSendState.EMAIL_GATED
                return result

            # Check page content for expired messages (only when no auth
            # fields found — authenticated pages still have "no longer
            # available" text hidden in the DOM).  Only count it as
            # expired if the element is actually *visible* on screen.
            try:
                expired_locator = page.locator(
                    "text=/link.*expired|no longer available|not available|not found/i"
                ).first
                if await expired_locator.is_visible(timeout=2000):
                    expired_text = await expired_locator.text_content(timeout=2000)
                    if expired_text:
                        result.state = DocSendState.LINK_EXPIRED
                        return result
            except Exception:
                pass

        # Check for folder/space (Download as ZIP or per-file Download buttons)
        try:
            zip_btn = page.locator('button[aria-label="Download as ZIP"]').first
            if await zip_btn.is_visible(timeout=2000):
                result.state = DocSendState.FOLDER
                return result
        except Exception:
            pass
        try:
            file_dl_btn = page.locator('button[aria-label="Download file"]').first
            if await file_dl_btn.is_visible(timeout=2000):
                result.state = DocSendState.FOLDER
                return result
        except Exception:
            pass

        # Check for download button
        download_selectors = [
            '[data-testid="download"]',
            ".download-button",
            'button:has-text("Download")',
            'a:has-text("Download PDF")',
            '[class*="download"]',
        ]

        for selector in download_selectors:
            try:
                download_btn = page.locator(selector).first
                if await download_btn.is_visible(timeout=2000):
                    result.state = DocSendState.DOWNLOAD_ENABLED
                    break
            except Exception:
                continue

        # If no download button found, check if we can access slides
        if result.state == DocSendState.UNKNOWN:
            # Check for page indicator (indicates viewable content)
            page_count = await get_slide_count(page)
            if page_count > 0:
                result.page_count = page_count
                result.state = DocSendState.DOWNLOAD_DISABLED
            else:
                # Try waiting a bit more for content to load
                await asyncio.sleep(2)
                page_count = await get_slide_count(page)
                if page_count > 0:
                    result.page_count = page_count
                    result.state = DocSendState.DOWNLOAD_DISABLED

    except Exception as e:
        result.error = str(e)

    return result


async def download_docsend_pdf(
    page: Page,
    output_dir: Path,
    filename: str,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> Path | None:
    """Download PDF directly when download is enabled.

    Args:
        page: Playwright page on the DocSend document
        output_dir: Directory to save the PDF
        filename: Filename for the PDF (without extension)
        config: Scraping configuration

    Returns:
        Path to downloaded PDF or None if download failed
    """
    download_selectors = [
        '[data-testid="download"]',
        ".download-button",
        'button:has-text("Download")',
        'a:has-text("Download PDF")',
        'a:has-text("Download")',
    ]

    for selector in download_selectors:
        try:
            download_btn = page.locator(selector).first
            if await download_btn.is_visible(timeout=2000):
                async with page.expect_download(timeout=60000) as download_info:
                    await download_btn.click()

                download = await download_info.value
                save_path = output_dir / f"{filename}.pdf"
                await download.save_as(save_path)
                return save_path
        except PlaywrightTimeout:
            continue
        except Exception:
            continue

    return None


async def _intercept_cloudfront_download(
    page: Page,
    click_locator,
    timeout_seconds: int = 180,
) -> tuple[str | None, str | None]:
    """Click a button and intercept the resulting CloudFront download URL.

    Returns:
        Tuple of (download_url, suggested_filename) or (None, None).
    """
    download_url = None
    suggested_filename = None

    def on_response(response):
        nonlocal download_url, suggested_filename
        # Skip Dropbox analytics noise
        if "dropbox.com/log" in response.url:
            return
        content_disp = response.headers.get("content-disposition", "")
        if "cloudfront.net" in response.url and "attachment" in content_disp:
            download_url = response.url
            fn_match = re.search(r'filename="?([^";\n]+)"?', content_disp)
            if fn_match:
                suggested_filename = fn_match.group(1)

    page.on("response", on_response)

    await click_locator.click()

    for _ in range(timeout_seconds // 3):
        if download_url:
            break
        await asyncio.sleep(3)

    page.remove_listener("response", on_response)
    return download_url, suggested_filename


async def download_folder(
    page: Page,
    output_dir: Path,
    filename: str,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> Path | None:
    """Download a DocSend folder/space.

    Handles two folder variants:
    1. ZIP download: single "Download as ZIP" button generates a server-side
       ZIP archive and delivers it via CloudFront.
    2. Per-file download: individual "Download file" buttons on each document
       row, each delivering a file via CloudFront.

    Args:
        page: Playwright page on the DocSend folder (post-auth)
        output_dir: Directory to save the output
        filename: Base filename (without extension)
        config: Scraping configuration

    Returns:
        Path to downloaded ZIP/directory or None if download failed
    """
    import httpx

    # --- Variant 1: Download as ZIP ---
    try:
        zip_btn = page.locator('button[aria-label="Download as ZIP"]').first
        if await zip_btn.is_visible(timeout=3000):
            url, fname = await _intercept_cloudfront_download(page, zip_btn)
            if not url:
                return None

            save_path = output_dir / f"{filename}.zip"
            try:
                async with httpx.AsyncClient(follow_redirects=True, timeout=300) as client:
                    async with client.stream("GET", url) as resp:
                        resp.raise_for_status()
                        with open(save_path, "wb") as f:
                            async for chunk in resp.aiter_bytes(chunk_size=65536):
                                f.write(chunk)
                return save_path
            except Exception:
                if save_path.exists():
                    save_path.unlink()
                return None
    except Exception:
        pass

    # --- Variant 2: Per-file downloads ---
    dl_buttons = page.locator('button[aria-label="Download file"]')
    count = await dl_buttons.count()
    if count == 0:
        return None
    downloaded = []

    async with httpx.AsyncClient(follow_redirects=True, timeout=120) as client:
        for i in range(count):
            btn = dl_buttons.nth(i)
            try:
                url, fname = await _intercept_cloudfront_download(page, btn, timeout_seconds=60)
                if not url:
                    continue

                safe_fname = fname or f"file_{i + 1}"
                # Sanitize filename
                safe_fname = safe_fname.replace("/", "_").replace("\\", "_")
                save_path = output_dir / safe_fname

                async with client.stream("GET", url) as resp:
                    resp.raise_for_status()
                    with open(save_path, "wb") as f:
                        async for chunk in resp.aiter_bytes(chunk_size=65536):
                            f.write(chunk)

                downloaded.append(save_path)
            except Exception:
                pass

            # Brief pause between downloads
            await asyncio.sleep(1)

    if not downloaded:
        return None

    # Return the output directory itself as the "path"
    return output_dir


async def scrape_slides(
    page: Page,
    company: str,
    output_dir: Path,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> ScrapeResult:
    """Scrape slides when download is disabled (view-only mode).

    This function assumes the page is already authenticated and showing the document.

    Args:
        page: Playwright page on the DocSend document
        company: Company name for the output
        output_dir: Directory to save output
        config: Scraping configuration

    Returns:
        ScrapeResult with status and details
    """
    result = ScrapeResult(url=page.url, company=company)

    # Get slide count
    total_pages = 0
    for attempt in range(config.max_retries):
        total_pages = await get_slide_count(page)
        if total_pages > 0:
            break
        await asyncio.sleep(2)

    if total_pages == 0:
        result.status = ScrapeStatus.NO_SLIDES
        result.error = "Could not determine slide count"
        return result

    result.total_pages = total_pages

    # Create output directory
    company_dir = output_dir / company.replace(" ", "_").replace("/", "_")
    company_dir.mkdir(parents=True, exist_ok=True)

    # Fetch all slide URLs via API
    slide_urls, fetch_failed = await fetch_slide_urls(page, total_pages, company, config)
    valid_urls = [u for u in slide_urls if u]

    if len(valid_urls) == 0:
        result.status = ScrapeStatus.ERROR
        result.error = "Failed to fetch any slide URLs"
        result.failed_slides = fetch_failed
        return result

    # Download images
    downloaded, download_failed = await download_images(slide_urls, company_dir, company, config)
    result.downloaded = len(downloaded)
    result.failed_slides = list(set(fetch_failed + download_failed))

    # Create PDF even with partial results
    if downloaded:
        pdf_path = company_dir / f"{company.replace(' ', '_')}.pdf"
        await create_pdf(downloaded, pdf_path)
        result.pdf_path = str(pdf_path)

    # Determine final status
    if len(downloaded) == total_pages:
        result.status = ScrapeStatus.SUCCESS
    elif len(downloaded) >= total_pages * config.min_success_ratio:
        result.status = ScrapeStatus.PARTIAL
        result.error = f"Got {len(downloaded)}/{total_pages} slides"
    elif len(downloaded) > 0:
        result.status = ScrapeStatus.PARTIAL
        result.error = f"Only got {len(downloaded)}/{total_pages} slides"
    else:
        result.status = ScrapeStatus.ERROR
        result.error = "All downloads failed"

    return result


async def route_docsend(
    url: str,
    company: str,
    output_dir: Path,
    password: str | None = None,
    email: str = DEFAULT_EMAIL,
    semaphore: asyncio.Semaphore | None = None,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> ScrapeResult:
    """Route a single DocSend URL to the appropriate handler.

    This function:
    1. Creates a cloud browser session
    2. Navigates to the URL
    3. Detects the page state
    4. Routes to the appropriate handler based on state

    Args:
        url: DocSend URL to process
        company: Company name for output organization
        output_dir: Directory to save output files
        password: Optional password (pre-extracted from email)
        email: Email to use for email-gated documents
        semaphore: Optional semaphore for concurrency control
        config: Scraping configuration

    Returns:
        ScrapeResult with status and details
    """

    async def _route():
        result = ScrapeResult(url=url, company=company)

        api_key = browser_use_api_key()
        playwright_browser = None

        try:
            prepare_playwright_tls()
            async with async_playwright() as p:
                playwright_browser = await p.chromium.connect_over_cdp(
                    browser_use_ws_url(api_key, config)
                )

                contexts = playwright_browser.contexts
                if contexts:
                    context = contexts[0]
                    pages = context.pages
                    page = pages[0] if pages else await context.new_page()
                else:
                    context = await playwright_browser.new_context()
                    page = await context.new_page()

                # Navigate to URL
                await asyncio.sleep(15)
                try:
                    response = await page.goto(
                        url, wait_until="networkidle", timeout=config.page_load_timeout
                    )
                except PlaywrightTimeout:
                    result.status = ScrapeStatus.TIMEOUT
                    result.error = "Page load timeout"
                    return result

                # Dismiss cookie consent if present
                try:
                    accept_btn = page.locator('button:has-text("Accept All")').first
                    if await accept_btn.is_visible(timeout=2000):
                        await accept_btn.click()
                        await asyncio.sleep(1)
                except Exception:
                    pass

                if DOCSEND_DEBUG_DIR:
                    debug_dir = Path(DOCSEND_DEBUG_DIR)
                    debug_dir.mkdir(parents=True, exist_ok=True)
                    debug_prefix = company.replace(" ", "_").replace("/", "_") or "unknown"
                    await page.screenshot(
                        path=str(debug_dir / f"{debug_prefix}.png"), full_page=True
                    )
                    html_path = debug_dir / f"{debug_prefix}.html"
                    html_path.write_text(await page.content())

                # Detect state
                state_result = await detect_docsend_state(
                    page,
                    url,
                    password,
                    response.status if response else None,
                    config,
                )

                # Route based on state
                match state_result.state:
                    case DocSendState.LINK_EXPIRED:
                        result.status = ScrapeStatus.LINK_EXPIRED
                        return result

                    case DocSendState.BLOCKED:
                        result.status = ScrapeStatus.ERROR
                        result.error = state_result.error or "Blocked by CloudFront/WAF"
                        return result

                    case DocSendState.PASSWORD_REQUIRED:
                        result.status = ScrapeStatus.PASSWORD_REQUIRED
                        result.error = (
                            state_result.error or "Password required but not provided or rejected"
                        )
                        return result

                    case DocSendState.EMAIL_GATED:
                        # Enter email and re-triage
                        success = await enter_email(page, email, config)
                        if not success:
                            # Email entry failed — check if the page is actually
                            # expired. Expired pages can show an email field too,
                            # which is indistinguishable from a real gate before
                            # attempting entry.
                            try:
                                expired_el = page.locator(
                                    "text=/link.*expired|no longer available|not found/i"
                                ).first
                                expired_text = await expired_el.text_content(timeout=2000)
                                if expired_text:
                                    result.status = ScrapeStatus.LINK_EXPIRED
                                    return result
                            except Exception:
                                pass
                            result.status = ScrapeStatus.AUTH_FAILED
                            result.error = "Failed to enter email"
                            return result

                        # Re-detect state after email entry
                        state_result = await detect_docsend_state(page, url, password, None, config)

                        if state_result.state == DocSendState.FOLDER:
                            # Folder — download as ZIP
                            company_dir = output_dir / company.replace(" ", "_").replace("/", "_")
                            company_dir.mkdir(parents=True, exist_ok=True)
                            zip_path = await download_folder(
                                page, company_dir, company.replace(" ", "_"), config
                            )
                            if zip_path:
                                result.status = ScrapeStatus.SUCCESS
                                result.pdf_path = str(zip_path)
                            else:
                                result.status = ScrapeStatus.ERROR
                                result.error = "Folder ZIP download failed"
                            return result
                        elif state_result.state == DocSendState.DOWNLOAD_ENABLED:
                            # Download directly
                            company_dir = output_dir / company.replace(" ", "_").replace("/", "_")
                            company_dir.mkdir(parents=True, exist_ok=True)
                            pdf_path = await download_docsend_pdf(
                                page, company_dir, company.replace(" ", "_"), config
                            )
                            if pdf_path:
                                result.status = ScrapeStatus.SUCCESS
                                result.pdf_path = str(pdf_path)
                            else:
                                # Fall back to scraping
                                return await scrape_slides(page, company, output_dir, config)
                        else:
                            # Scrape slides
                            return await scrape_slides(page, company, output_dir, config)

                    case DocSendState.FOLDER:
                        # Folder already authenticated — download as ZIP
                        company_dir = output_dir / company.replace(" ", "_").replace("/", "_")
                        company_dir.mkdir(parents=True, exist_ok=True)
                        zip_path = await download_folder(
                            page, company_dir, company.replace(" ", "_"), config
                        )
                        if zip_path:
                            result.status = ScrapeStatus.SUCCESS
                            result.pdf_path = str(zip_path)
                        else:
                            result.status = ScrapeStatus.ERROR
                            result.error = "Folder ZIP download failed"
                        return result

                    case DocSendState.DOWNLOAD_ENABLED:
                        # Download directly
                        company_dir = output_dir / company.replace(" ", "_").replace("/", "_")
                        company_dir.mkdir(parents=True, exist_ok=True)
                        pdf_path = await download_docsend_pdf(
                            page, company_dir, company.replace(" ", "_"), config
                        )
                        if pdf_path:
                            result.status = ScrapeStatus.SUCCESS
                            result.pdf_path = str(pdf_path)
                            return result
                        else:
                            # Fall back to slide scraping
                            return await scrape_slides(page, company, output_dir, config)

                    case DocSendState.DOWNLOAD_DISABLED:
                        # Scrape slides
                        return await scrape_slides(page, company, output_dir, config)

                    case DocSendState.UNKNOWN:
                        # Try scraping anyway
                        return await scrape_slides(page, company, output_dir, config)

                return result

        except Exception as e:
            result.status = ScrapeStatus.ERROR
            result.error = redact_browser_use_secret(e, api_key)
            return result

        finally:
            if playwright_browser:
                try:
                    await playwright_browser.close()
                except Exception:
                    pass

    if semaphore:
        async with semaphore:
            return await _route()
    else:
        return await _route()


async def route_all_docsends(
    urls: list[dict],
    output_dir: Path,
    email: str = DEFAULT_EMAIL,
    max_parallel: int = MAX_PARALLEL,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> list[ScrapeResult]:
    """Route multiple DocSend URLs to appropriate handlers.

    Args:
        urls: List of dicts with 'url', 'company', and optional 'password' keys
        output_dir: Directory to save output files
        email: Email to use for email-gated documents
        max_parallel: Maximum number of parallel browser sessions
        config: Scraping configuration

    Returns:
        List of ScrapeResult objects
    """
    if not urls:
        return []

    output_dir.mkdir(parents=True, exist_ok=True)

    semaphore = asyncio.Semaphore(max_parallel)

    tasks = [
        route_docsend(
            url=u["url"],
            company=u.get("company", "Unknown"),
            output_dir=output_dir,
            password=u.get("password"),
            email=email,
            semaphore=semaphore,
            config=config,
        )
        for u in urls
    ]

    results = await asyncio.gather(*tasks, return_exceptions=True)

    # Convert exceptions to ScrapeResults
    processed_results = []
    for i, r in enumerate(results):
        if isinstance(r, ScrapeResult):
            processed_results.append(r)
        else:
            # Exception occurred
            processed_results.append(
                ScrapeResult(
                    url=urls[i]["url"],
                    company=urls[i].get("company", "Unknown"),
                    status=ScrapeStatus.ERROR,
                    error=str(r),
                )
            )

    return processed_results
