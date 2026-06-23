#!/usr/bin/env python3
"""
DocSend Scraper using browser-use CLOUD + Playwright control.

Strategy:
1. Create a browser session via browser-use cloud API (gets CDP URL)
2. Connect Playwright to the cloud browser via CDP
3. Run deterministic Playwright automation (no LLM needed)
4. Use DocSend's /page_data/ API to get slide URLs
5. Download images and compile to PDF

This gives us:
- Cloud browsers (no local display needed)
- No headless detection issues (browser-use handles this)
- Deterministic control via Playwright (no LLM costs)
"""

import asyncio
import os
import re
from dataclasses import dataclass, field
from enum import Enum
from io import BytesIO
from pathlib import Path
from typing import Optional
from urllib.parse import urlencode

import httpx
from browser_use_sdk import AsyncBrowserUse
from PIL import Image
from playwright.async_api import TimeoutError as PlaywrightTimeout
from playwright.async_api import async_playwright

from centaur_sdk import secret

# Configuration
DEFAULT_EMAIL = ""
API_KEY = secret("BROWSER_USE_API_KEY", "")
BROWSER_USE_PROXY_COUNTRY = os.environ.get("BROWSER_USE_PROXY_COUNTRY", "")  # noqa: TID251
BROWSER_USE_PROFILE_ID = os.environ.get("BROWSER_USE_PROFILE_ID", "")  # noqa: TID251


def browser_use_api_key() -> str:
    # Server mode returns the stub key name; the proxy swaps it for the real
    # key in the query string (match_query). Local mode returns the env value.
    return secret("BROWSER_USE_API_KEY", "")


def prepare_playwright_tls() -> None:
    cert_path = "/firewall-certs/ca-cert.pem"
    if os.path.exists(cert_path) and not os.environ.get("NODE_EXTRA_CA_CERTS"):  # noqa: TID251
        os.environ["NODE_EXTRA_CA_CERTS"] = cert_path
    node_options = os.environ.get("NODE_OPTIONS", "")  # noqa: TID251
    if "--use-system-ca" not in node_options.split():
        os.environ["NODE_OPTIONS"] = f"{node_options} --use-system-ca".strip()


def browser_use_ws_url(api_key: str, config: "ScrapeConfig") -> str:
    params = {
        "apiKey": api_key,
        "browserScreenWidth": str(config.browser_width),
        "browserScreenHeight": str(config.browser_height),
    }
    if BROWSER_USE_PROXY_COUNTRY:
        params["proxyCountryCode"] = BROWSER_USE_PROXY_COUNTRY.lower()
    if BROWSER_USE_PROFILE_ID:
        params["profileId"] = BROWSER_USE_PROFILE_ID
    return f"wss://connect.browser-use.com?{urlencode(params)}"


def redact_browser_use_secret(error: Exception, api_key: str) -> str:
    text = str(error)
    if api_key:
        text = text.replace(api_key, "<redacted>")
    return re.sub(r"apiKey=[^&\\s]+", "apiKey=<redacted>", text)


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


class ScrapeStatus(Enum):
    """Possible scrape outcomes."""
    SUCCESS = "success"
    PARTIAL = "partial"  # Some slides failed but got most
    PASSWORD_REQUIRED = "password_required"
    LINK_EXPIRED = "link_expired"
    NO_SLIDES = "no_slides"
    AUTH_FAILED = "auth_failed"
    TIMEOUT = "timeout"
    ERROR = "error"


@dataclass
class ScrapeConfig:
    """Configuration for scraping behavior."""
    # Timeouts (in ms)
    page_load_timeout: int = 45000
    auth_timeout: int = 5000  # Short timeout - email field should appear quickly
    network_idle_timeout: int = 20000

    # Retries
    max_retries: int = 3
    retry_delay: float = 2.0  # seconds

    # Browser session
    browser_timeout: int = 240  # seconds (max 240)
    browser_width: int = 1920
    browser_height: int = 1080

    # Thresholds
    min_success_ratio: float = 0.5  # Consider partial success if > 50% slides


@dataclass
class ScrapeResult:
    """Result of a scrape operation."""
    url: str
    company: str
    status: ScrapeStatus = ScrapeStatus.ERROR
    total_pages: int = 0
    downloaded: int = 0
    failed_slides: list = field(default_factory=list)
    error: Optional[str] = None
    pdf_path: Optional[str] = None

    def to_dict(self) -> dict:
        return {
            "url": self.url,
            "company": self.company,
            "status": self.status.value,
            "total_pages": self.total_pages,
            "downloaded": self.downloaded,
            "failed_slides": self.failed_slides,
            "error": self.error,
            "pdf_path": self.pdf_path,
        }


async def retry_async(
    func,
    max_retries: int = 3,
    delay: float = 2.0,
    exceptions: tuple = (Exception,),
    on_retry: callable = None,
):
    """Retry an async function with exponential backoff."""
    last_exception = None
    for attempt in range(max_retries):
        try:
            return await func()
        except exceptions as e:
            last_exception = e
            if attempt < max_retries - 1:
                wait_time = delay * (2 ** attempt)  # Exponential backoff
                if on_retry:
                    on_retry(attempt + 1, max_retries, e, wait_time)
                await asyncio.sleep(wait_time)
    raise last_exception


# Default config
DEFAULT_CONFIG = ScrapeConfig()


async def enter_password(page, password: str, config: ScrapeConfig = DEFAULT_CONFIG) -> bool:
    """Enter password on a password-protected DocSend page.

    Handles both standalone password forms and combined email+password forms
    (DocSend uses type="text" passcode fields, not type="password").

    Returns:
        True if password was accepted, False otherwise.
    """
    try:
        # DocSend passcode fields use type="text", not type="password"
        pw_selectors = [
            'input[type="password"]',
            '#link_auth_form_passcode',
            'input[name*="passcode"]',
        ]

        password_field = None
        for sel in pw_selectors:
            loc = page.locator(sel).first
            try:
                await loc.wait_for(state='visible', timeout=3000)
                password_field = loc
                break
            except Exception:
                continue

        if not password_field:
            return False

        # If email field is on the same form, fill it first.
        # Use form-scoped selector to avoid matching the hidden
        # feedback/chat email field (#feedback_sender_email).
        # Includes folder-style ReactModal email fields.
        email_selectors = [
            '#link_auth_form_email',
            '#new_link_auth_form input[type="email"]',
            '.js-auth-form_email-field',
            '.ReactModal__Content input[type="email"]',
            '#email[type="email"]',
        ]
        for em_sel in email_selectors:
            try:
                email_input = page.locator(em_sel).first
                await email_input.wait_for(state='visible', timeout=2000)
                await email_input.fill(DEFAULT_EMAIL)
                await asyncio.sleep(0.5)
                break
            except Exception:
                continue

        await password_field.fill(password)
        await asyncio.sleep(0.5)

        # Try to find and click submit button
        submit_selectors = [
            'button[type="submit"]',
            'button:has-text("Submit")',
            'button:has-text("Continue")',
            'button:has-text("Confirm")',
            'button:has-text("Enter")',
        ]

        clicked = False
        for selector in submit_selectors:
            try:
                btn = page.locator(selector).first
                if await btn.is_visible(timeout=1000):
                    await btn.click()
                    clicked = True
                    break
            except Exception:
                continue

        if not clicked:
            await page.keyboard.press('Enter')

        # Wait for navigation/load
        await page.wait_for_load_state('networkidle', timeout=config.network_idle_timeout)
        await asyncio.sleep(2)
        # Check if still on password/passcode page (single-doc form or folder modal)
        still_on_auth = await page.query_selector(
            'input[type="password"], #link_auth_form_passcode, input[name*="passcode"]'
        )
        if still_on_auth:
            # Could be a hidden field after modal dismissed — check visibility
            try:
                is_visible = await page.locator(
                    'input[type="password"], #link_auth_form_passcode, input[name*="passcode"]'
                ).first.is_visible(timeout=1000)
                if is_visible:
                    return False
            except Exception:
                pass

        return True

    except PlaywrightTimeout:
        return False
    except Exception:
        return False


async def get_slide_count(page) -> int:
    """Get total slide count from page indicator."""
    selectors = ['.toolbar-page-indicator', '.page-label', '[class*="page-indicator"]']
    for selector in selectors:
        try:
            indicator = await page.query_selector(selector)
            if indicator:
                text = await indicator.text_content()
                match = re.search(r'(\d+)\s*/\s*(\d+)', text)
                if match:
                    return int(match.group(2))
        except Exception:
            continue
    return 0


async def fetch_slide_urls(
    page,
    total_pages: int,
    company: str,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> tuple[list[str], list[int]]:
    """Fetch all slide image URLs via the page_data API.

    Returns:
        Tuple of (urls list, failed slide numbers)
    """
    urls = []
    failed = []
    base_url = page.url.split('?')[0]

    for i in range(1, total_pages + 1):
        url = None
        last_error = None

        for attempt in range(config.max_retries):
            try:
                result = await page.evaluate(f"""
                    (async () => {{
                        const resp = await fetch('{base_url}/page_data/{i}');
                        if (!resp.ok) return {{error: 'HTTP ' + resp.status}};
                        const text = await resp.text();
                        if (!text) return {{error: 'Empty response'}};
                        const data = JSON.parse(text);
                        return {{url: data.imageUrl || data.directImageUrl || null}};
                    }})()
                """)

                if result and result.get('url'):
                    url = result['url']
                    break
                elif result and result.get('error'):
                    last_error = result['error']
                    if 'HTTP 4' in last_error:  # 4xx errors - don't retry
                        break
            except Exception as e:
                last_error = str(e)

            if attempt < config.max_retries - 1:
                await asyncio.sleep(config.retry_delay)

        if url:
            urls.append(url)
        else:
            urls.append(None)
            failed.append(i)

    return urls, failed


async def download_images(
    urls: list[str],
    output_dir: Path,
    company: str,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> tuple[list[Path], list[int]]:
    """Download images from URLs with retry logic.

    Returns:
        Tuple of (downloaded paths, failed slide numbers)
    """
    downloaded = []
    failed = []

    async with httpx.AsyncClient(timeout=30.0) as http:
        for i, url in enumerate(urls, 1):
            if not url:
                failed.append(i)
                continue

            success = False

            for attempt in range(config.max_retries):
                try:
                    response = await http.get(url)
                    response.raise_for_status()

                    img_path = output_dir / f"slide_{i:03d}.png"
                    img = Image.open(BytesIO(response.content))
                    img.save(img_path, "PNG")
                    downloaded.append(img_path)
                    success = True
                    break

                except httpx.HTTPStatusError as e:
                    if e.response.status_code in (401, 403, 404):
                        # Don't retry auth/not found errors
                        break
                except Exception:
                    pass

                if attempt < config.max_retries - 1:
                    await asyncio.sleep(config.retry_delay)

            if not success:
                failed.append(i)

    return downloaded, failed


async def create_pdf(image_paths: list[Path], output_path: Path):
    """Create PDF from images."""
    if not image_paths:
        return

    images = [Image.open(p) for p in sorted(image_paths)]
    rgb_images = [img.convert('RGB') if img.mode != 'RGB' else img for img in images]

    if rgb_images:
        rgb_images[0].save(
            output_path, "PDF", save_all=True,
            append_images=rgb_images[1:] if len(rgb_images) > 1 else []
        )


async def scrape_docsend(
    url: str,
    company: str,
    output_dir: Path,
    email: str,
    semaphore: asyncio.Semaphore,
    password: str | None = None,
    config: ScrapeConfig = DEFAULT_CONFIG,
) -> ScrapeResult:
    """Scrape a single DocSend URL using cloud browser + Playwright.

    Args:
        url: DocSend URL to scrape
        company: Company name for output organization
        output_dir: Directory to save output files
        email: Email to use for email-gated documents
        semaphore: Semaphore for concurrency control
        password: Optional password for password-protected documents
        config: Scraping configuration
    """

    async with semaphore:
        result = ScrapeResult(url=url, company=company)

        # Create output directory
        company_dir = output_dir / company.replace(" ", "_").replace("/", "_")
        company_dir.mkdir(parents=True, exist_ok=True)

        api_key = browser_use_api_key()
        if not api_key:
            result.status = ScrapeStatus.ERROR
            result.error = "BROWSER_USE_API_KEY not configured"
            return result
        playwright_browser = None

        try:
            prepare_playwright_tls()
            async with async_playwright() as p:
                playwright_browser = await p.chromium.connect_over_cdp(
                    browser_use_ws_url(api_key, config)
                )

                # Get the default context and page
                contexts = playwright_browser.contexts
                if contexts:
                    context = contexts[0]
                    pages = context.pages
                    page = pages[0] if pages else await context.new_page()
                else:
                    context = await playwright_browser.new_context()
                    page = await context.new_page()

                # Step 3: Navigate to DocSend URL with retry

                async def navigate():
                    await page.goto(url, wait_until='networkidle', timeout=config.page_load_timeout)

                try:
                    await retry_async(
                        navigate,
                        max_retries=config.max_retries,
                        delay=config.retry_delay,
                        exceptions=(PlaywrightTimeout, Exception),
                    )
                except PlaywrightTimeout:
                    result.status = ScrapeStatus.TIMEOUT
                    result.error = "Page load timeout"
                    return result
                except Exception as e:
                    result.status = ScrapeStatus.ERROR
                    result.error = f"Navigation failed: {e}"
                    return result

                # Check for error states
                title = await page.title()
                if '404' in title or 'not found' in title.lower():
                    result.status = ScrapeStatus.LINK_EXPIRED
                    return result

                # Check for password field
                password_field = await page.query_selector('input[type="password"]')
                if password_field:
                    if password:
                        success = await enter_password(page, password, config)
                        if not success:
                            result.status = ScrapeStatus.PASSWORD_REQUIRED
                            result.error = "Password rejected or entry failed"
                            return result
                    else:
                        result.status = ScrapeStatus.PASSWORD_REQUIRED
                        return result

                # Dismiss cookie consent if present
                try:
                    accept_btn = page.locator('button:has-text("Accept All")').first
                    if await accept_btn.is_visible(timeout=2000):
                        await accept_btn.click()
                        await asyncio.sleep(1)
                except Exception:
                    pass

                # Check for email prompt - always try to auth if present
                # (some documents show preview but need auth for API access)
                email_field = None
                try:
                    email_field = page.locator('#prompt input[type="email"]').first
                    await email_field.wait_for(state='visible', timeout=config.auth_timeout)
                except Exception:
                    # Try fallback - find visible email input (not the feedback one)
                    try:
                        # Look for email input in a modal/prompt container
                        modal_email = page.locator('.modal input[type="email"], #prompt input[type="email"], [class*="auth"] input[type="email"]').first
                        if await modal_email.count() > 0:
                            box = await modal_email.bounding_box(timeout=2000)
                            if box and box['width'] > 50:
                                email_field = modal_email
                    except Exception:
                        pass

                if email_field:
                    try:
                        await email_field.click(timeout=5000)
                        await email_field.fill(email, timeout=5000)

                        # Click continue button
                        submit_btn = page.locator('button:has-text("Continue")').first
                        try:
                            if await submit_btn.is_visible(timeout=2000):
                                await submit_btn.click(timeout=5000)
                            else:
                                await page.keyboard.press('Enter')
                        except Exception:
                            await page.keyboard.press('Enter')

                        # Wait for document to load
                        await page.wait_for_load_state('networkidle', timeout=config.network_idle_timeout)
                        await asyncio.sleep(2)
                    except PlaywrightTimeout:
                        pass
                    except Exception:
                        pass

                # Get slide count with retry
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

        except PlaywrightTimeout as e:
            result.status = ScrapeStatus.TIMEOUT
            result.error = str(e)

        except Exception as e:
            result.status = ScrapeStatus.ERROR
            result.error = redact_browser_use_secret(e, api_key)

        finally:
            # Clean up
            if playwright_browser:
                try:
                    await playwright_browser.close()
                except Exception:
                    pass

        return result
