from __future__ import annotations


def test_docs_text_from_document_extracts_body_and_tables():
    from api.integrations.gsuite.docs import docs_text_from_document

    doc = {
        "body": {
            "content": [
                {
                    "paragraph": {
                        "elements": [{"textRun": {"content": "Intro\n"}}],
                    },
                },
                {
                    "table": {
                        "tableRows": [
                            {
                                "tableCells": [
                                    {
                                        "content": [
                                            {
                                                "paragraph": {
                                                    "elements": [
                                                        {
                                                            "textRun": {
                                                                "content": "Cell"
                                                            }
                                                        }
                                                    ]
                                                }
                                            }
                                        ]
                                    }
                                ]
                            }
                        ]
                    }
                },
            ]
        }
    }

    assert docs_text_from_document(doc) == "Intro\nCell"


def test_docs_text_from_document_extracts_tabs():
    from api.integrations.gsuite.docs import docs_text_from_document

    doc = {
        "tabs": [
            {
                "documentTab": {
                    "body": {
                        "content": [
                            {
                                "paragraph": {
                                    "elements": [{"textRun": {"content": "Tab one"}}]
                                }
                            }
                        ]
                    }
                }
            },
            {
                "documentTab": {
                    "body": {
                        "content": [
                            {
                                "paragraph": {
                                    "elements": [{"textRun": {"content": "Tab two"}}]
                                }
                            }
                        ]
                    }
                }
            },
        ]
    }

    assert docs_text_from_document(doc) == "Tab one\nTab two"


def test_drive_readonly_client_lists_google_docs(monkeypatch):
    from api.integrations.gsuite import drive

    captured: dict[str, object] = {}

    class FakeListRequest:
        def execute(self):
            return {"files": [{"id": "doc-1"}]}

    class FakeFiles:
        def list(self, **kwargs):
            captured.update(kwargs)
            return FakeListRequest()

    class FakeDriveService:
        def files(self):
            return FakeFiles()

    monkeypatch.setattr(drive, "get_drive_service", lambda: FakeDriveService())

    client = drive.GoogleDriveReadonlyClient()
    result = client.list_docs(
        query="mimeType = 'application/vnd.google-apps.document'",
        page_size=25,
        page_token="next",
    )

    assert result == {"files": [{"id": "doc-1"}]}
    assert captured["pageSize"] == 25
    assert captured["pageToken"] == "next"
    assert captured["includeItemsFromAllDrives"] is True
    assert captured["supportsAllDrives"] is True
    assert "lastModifyingUser" in str(captured["fields"])
