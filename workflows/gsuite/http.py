from __future__ import annotations

import os
from urllib.parse import urlsplit


def build_http():
    """Build a google-api-python-client HTTP transport routed through iron-proxy.

    Google REST clients use httplib2, so we wire proxy and CA settings
    explicitly. The imports stay lazy so workflow modules can load in test
    environments that have not installed tool-only Google dependencies.
    """
    import httplib2
    import socks

    proxy_url = os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy")
    proxy_info = None
    if proxy_url:
        parts = urlsplit(proxy_url)
        proxy_info = httplib2.ProxyInfo(
            proxy_type=socks.PROXY_TYPE_HTTP,
            proxy_host=parts.hostname,
            proxy_port=parts.port or 8080,
        )
    ca_certs = os.environ.get("SSL_CERT_FILE") or os.environ.get(
        "REQUESTS_CA_BUNDLE"
    )
    return httplib2.Http(proxy_info=proxy_info, ca_certs=ca_certs)
