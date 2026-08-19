"""Figma REST API client using personal access token."""

import json
import re
from dataclasses import dataclass, field
from urllib.request import Request, urlopen

from centaur_sdk import secret


@dataclass
class FigmaDesignSystem:
    """Extracted design system information from a Figma file."""

    file_name: str = ""
    colors: list[dict] = field(default_factory=list)
    text_styles: list[dict] = field(default_factory=list)
    components: list[dict] = field(default_factory=list)
    variables: list[dict] = field(default_factory=list)
    frames: list[dict] = field(default_factory=list)
    effects: list[dict] = field(default_factory=list)
    grids: list[dict] = field(default_factory=list)


class FigmaClient:
    """Client for Figma REST API using personal access token."""

    BASE_URL = "https://api.figma.com/v1"

    def __init__(self, token: str | None = None):
        self.token = token or secret("FIGMA_ACCESS_TOKEN", "")
        if not self.token:
            raise ValueError("Figma token not found. Set FIGMA_ACCESS_TOKEN or pass token.")

    def _request(self, endpoint: str, retries: int = 3) -> dict:
        """Make authenticated request to Figma API with retry on rate limit."""
        import time
        from urllib.error import HTTPError

        url = f"{self.BASE_URL}{endpoint}"
        req = Request(url, headers={"X-Figma-Token": self.token})

        for attempt in range(retries):
            try:
                with urlopen(req, timeout=60) as resp:
                    return json.loads(resp.read().decode())
            except HTTPError as e:
                if e.code == 429 and attempt < retries - 1:
                    wait = int(e.headers.get("Retry-After", 2**attempt))
                    time.sleep(wait)
                    continue
                raise

    @staticmethod
    def parse_url(url: str) -> tuple[str, str | None]:
        """Parse Figma URL to extract file key and optional node ID.

        Returns: (file_key, node_id) where node_id may be None
        """
        # Handle various Figma URL formats
        # https://www.figma.com/file/ABC123/Name?node-id=1:2
        # https://www.figma.com/design/ABC123/Name?node-id=1-2
        # https://www.figma.com/proto/ABC123/Name

        match = re.search(r"figma\.com/(?:file|design|proto)/([a-zA-Z0-9]+)", url)
        if not match:
            raise ValueError(f"Invalid Figma URL: {url}")

        file_key = match.group(1)

        # Extract node-id if present
        node_match = re.search(r"node-id=([0-9:-]+)", url)
        node_id = node_match.group(1).replace("-", ":") if node_match else None

        return file_key, node_id

    def get_file(self, file_key: str) -> dict:
        """Get full file data."""
        return self._request(f"/files/{file_key}")

    def get_file_styles(self, file_key: str) -> dict:
        """Get published styles from file."""
        return self._request(f"/files/{file_key}/styles")

    def get_file_components(self, file_key: str) -> dict:
        """Get published components from file."""
        return self._request(f"/files/{file_key}/components")

    def get_file_variables(self, file_key: str) -> dict:
        """Get local variables from file."""
        try:
            return self._request(f"/files/{file_key}/variables/local")
        except Exception:
            return {"variables": {}, "variableCollections": {}}

    def get_node(self, file_key: str, node_id: str) -> dict:
        """Get specific node(s) from file."""
        return self._request(f"/files/{file_key}/nodes?ids={node_id}")

    def crawl(self, url: str) -> FigmaDesignSystem:
        """Crawl a Figma file/frame URL and extract all design system info."""
        file_key, node_id = self.parse_url(url)

        # Get file data
        file_data = self.get_file(file_key)
        ds = FigmaDesignSystem(file_name=file_data.get("name", "Unknown"))

        # Extract from document tree
        document = file_data.get("document", {})
        self._extract_from_tree(document, ds)

        # Extract published styles
        styles_data = file_data.get("styles", {})
        for style_id, style in styles_data.items():
            style_type = style.get("styleType", "")
            if style_type == "FILL":
                ds.colors.append(
                    {
                        "id": style_id,
                        "name": style.get("name", ""),
                        "description": style.get("description", ""),
                    }
                )
            elif style_type == "TEXT":
                ds.text_styles.append(
                    {
                        "id": style_id,
                        "name": style.get("name", ""),
                        "description": style.get("description", ""),
                    }
                )
            elif style_type == "EFFECT":
                ds.effects.append(
                    {
                        "id": style_id,
                        "name": style.get("name", ""),
                        "description": style.get("description", ""),
                    }
                )
            elif style_type == "GRID":
                ds.grids.append(
                    {
                        "id": style_id,
                        "name": style.get("name", ""),
                        "description": style.get("description", ""),
                    }
                )

        # Try to get variables
        try:
            vars_data = self.get_file_variables(file_key)
            for var_id, var in vars_data.get("variables", {}).items():
                ds.variables.append(
                    {
                        "id": var_id,
                        "name": var.get("name", ""),
                        "type": var.get("resolvedType", ""),
                        "values": var.get("valuesByMode", {}),
                    }
                )
        except Exception:
            pass

        return ds

    def _extract_from_tree(self, node: dict, ds: FigmaDesignSystem, depth: int = 0) -> None:
        """Recursively extract design info from document tree."""
        node_type = node.get("type", "")
        node_name = node.get("name", "")

        # Extract components
        if node_type == "COMPONENT":
            ds.components.append(
                {
                    "id": node.get("id", ""),
                    "name": node_name,
                    "description": node.get("description", ""),
                }
            )

        # Extract frames (top-level screens/artboards)
        if node_type == "FRAME" and depth <= 2:
            fills = node.get("fills", [])
            ds.frames.append(
                {
                    "id": node.get("id", ""),
                    "name": node_name,
                    "width": node.get("absoluteBoundingBox", {}).get("width"),
                    "height": node.get("absoluteBoundingBox", {}).get("height"),
                    "background": self._extract_color(fills[0]) if fills else None,
                }
            )

        # Extract colors from fills
        for fill in node.get("fills", []):
            color = self._extract_color(fill)
            if color and color not in [c.get("value") for c in ds.colors]:
                ds.colors.append({"value": color, "source": node_name})

        # Extract text styles
        if node_type == "TEXT":
            style = node.get("style", {})
            if style:
                text_style = {
                    "source": node_name,
                    "fontFamily": style.get("fontFamily", ""),
                    "fontSize": style.get("fontSize", 0),
                    "fontWeight": style.get("fontWeight", 400),
                    "lineHeight": style.get("lineHeightPx"),
                    "letterSpacing": style.get("letterSpacing", 0),
                }
                # Dedupe by font+size
                family = text_style["fontFamily"]
                size = text_style["fontSize"]
                weight = text_style["fontWeight"]
                key = f"{family}-{size}-{weight}"
                existing_keys = [
                    f"{t.get('fontFamily')}-{t.get('fontSize')}-{t.get('fontWeight')}"
                    for t in ds.text_styles
                ]
                if key not in existing_keys:
                    ds.text_styles.append(text_style)

        # Recurse into children
        for child in node.get("children", []):
            self._extract_from_tree(child, ds, depth + 1)

    def _extract_color(self, fill: dict) -> str | None:
        """Extract hex color from fill."""
        if fill.get("type") != "SOLID":
            return None
        color = fill.get("color", {})
        if not color:
            return None
        r = int(color.get("r", 0) * 255)
        g = int(color.get("g", 0) * 255)
        b = int(color.get("b", 0) * 255)
        a = color.get("a", 1)
        if a < 1:
            return f"rgba({r}, {g}, {b}, {a:.2f})"
        return f"#{r:02x}{g:02x}{b:02x}"


def _client() -> FigmaClient:
    return FigmaClient()
