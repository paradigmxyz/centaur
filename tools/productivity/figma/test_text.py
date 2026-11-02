"""Slide text and URL parsing tests for the Figma tool.

Run from this directory:
    uv run --no-project --python 3.11 --with pytest pytest test_text.py
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

TOOL_DIR = Path(__file__).parent
REPO_ROOT = TOOL_DIR.parents[2]
sys.path.insert(0, str(REPO_ROOT))


def _load_client_module():
    client_path = TOOL_DIR / "client.py"
    spec = importlib.util.spec_from_file_location("test_figma_text_module", client_path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _text(node_id: str, characters: str, x: float, y: float, **extra) -> dict:
    return {
        "id": node_id,
        "type": "TEXT",
        "characters": characters,
        "absoluteBoundingBox": {"x": x, "y": y, "width": 100, "height": 20},
        **extra,
    }


def _client_for(document: dict):
    client = _load_client_module().FigmaClient(token="placeholder-token")
    requested_keys: list[str] = []

    def fake_get_file(file_key: str) -> dict:
        requested_keys.append(file_key)
        return {"name": "Deck", "document": document}

    client.get_file = fake_get_file
    return client, requested_keys


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("https://www.figma.com/slides/AbC123xyz/Q3-update?node-id=4-12", ("AbC123xyz", "4:12")),
        ("https://www.figma.com/board/AbC123xyz/Retro", ("AbC123xyz", None)),
        ("https://www.figma.com/design/AbC123xyz/App?node-id=1-2", ("AbC123xyz", "1:2")),
    ],
)
def test_parse_url_accepts_slides_board_and_design(url: str, expected: tuple) -> None:
    assert _load_client_module().FigmaClient.parse_url(url) == expected


def test_parse_url_rejects_non_file_urls() -> None:
    with pytest.raises(ValueError, match="Invalid Figma URL"):
        _load_client_module().FigmaClient.parse_url("https://www.figma.com/community/plugins")


def test_get_text_reads_slides_in_deck_order_and_text_top_to_bottom() -> None:
    document = {
        "type": "DOCUMENT",
        "children": [
            {
                "type": "CANVAS",
                "children": [
                    {
                        "type": "SLIDE_GRID",
                        "children": [
                            {
                                "type": "SLIDE_ROW",
                                "children": [
                                    {
                                        "id": "1:1",
                                        "type": "SLIDE",
                                        "name": "Title",
                                        "children": [
                                            _text("1:3", "Raising $4M seed", x=0, y=300),
                                            _text("1:2", "Acme Robotics", x=0, y=100),
                                            _text("1:4", "Draft only", x=0, y=50, visible=False),
                                        ],
                                    },
                                    {
                                        "id": "2:1",
                                        "type": "SLIDE",
                                        "name": "Traction",
                                        "children": [
                                            {
                                                "type": "GROUP",
                                                "visible": False,
                                                "children": [_text("2:3", "Old chart", 0, 0)],
                                            },
                                            _text("2:2", "40% MoM revenue growth", x=0, y=100),
                                        ],
                                    },
                                ],
                            }
                        ],
                    }
                ],
            }
        ],
    }
    client, requested_keys = _client_for(document)

    slides = client.get_text("https://www.figma.com/slides/AbC123xyz/Q3-update?node-id=4-12")

    assert requested_keys == ["AbC123xyz"]
    assert slides == [
        {"id": "1:1", "name": "Title", "text": ["Acme Robotics", "Raising $4M seed"]},
        {"id": "2:1", "name": "Traction", "text": ["40% MoM revenue growth"]},
    ]


def test_get_text_reads_design_file_frames_left_to_right() -> None:
    def frame(node_id: str, name: str, x: float, **extra) -> dict:
        return {
            "id": node_id,
            "type": "FRAME",
            "name": name,
            "absoluteBoundingBox": {"x": x, "y": 0, "width": 1920, "height": 1080},
            "children": [_text(f"{node_id}-t", f"{name} copy", x=x, y=40)],
            **extra,
        }

    document = {
        "type": "DOCUMENT",
        "children": [
            {
                "type": "CANVAS",
                "children": [
                    frame("3:1", "Problem", x=2000),
                    frame("3:2", "Cover", x=0),
                    frame("3:3", "Scratch", x=4000, visible=False),
                ],
            }
        ],
    }
    client, _ = _client_for(document)

    slides = client.get_text("https://www.figma.com/design/AbC123xyz/Deck")

    assert slides == [
        {"id": "3:2", "name": "Cover", "text": ["Cover copy"]},
        {"id": "3:1", "name": "Problem", "text": ["Problem copy"]},
    ]
