from __future__ import annotations

from typing import Any

from workflows.gsuite.http import build_http


def get_docs_service():
    """Return a proxy-authenticated Google Docs v1 service."""
    from googleapiclient.discovery import build

    return build("docs", "v1", http=build_http())


def docs_get(document_id: str, include_tabs: bool = True) -> dict[str, Any]:
    """Fetch a Google Doc with body/tab content."""
    service = get_docs_service()
    return (
        service.documents()
        .get(documentId=document_id, includeTabsContent=include_tabs)
        .execute()
    )


def extract_text_from_content(content: list[dict[str, Any]]) -> str:
    """Extract plain text from Google Docs structural content."""
    text_parts: list[str] = []
    for element in content:
        if "paragraph" in element:
            for para_element in element["paragraph"].get("elements", []):
                if "textRun" in para_element:
                    text_parts.append(para_element["textRun"].get("content", ""))
        elif "table" in element:
            for row in element["table"].get("tableRows", []):
                for cell in row.get("tableCells", []):
                    text_parts.append(extract_text_from_content(cell.get("content", [])))
    return "".join(text_parts)


def docs_text_from_document(doc: dict[str, Any]) -> str:
    """Extract plain text from a Google Docs API document response."""
    if doc.get("tabs"):
        all_text = []
        for tab in doc["tabs"]:
            doc_tab = tab.get("documentTab", {})
            body = doc_tab.get("body", {})
            all_text.append(extract_text_from_content(body.get("content", [])))
        return "\n".join(all_text)
    return extract_text_from_content(doc.get("body", {}).get("content", []))


def docs_get_text(document_id: str) -> str:
    """Return plain text content from a Google Doc."""
    return docs_text_from_document(docs_get(document_id))
