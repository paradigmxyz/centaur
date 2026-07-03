"""pond session-archive client - wraps the in-sandbox pond CLI.

pond archives the harness-native session logs (Claude Code, Codex, pi) from
every sandbox into one shared store and serves search over them. This client
shells out to the pond binary baked into the sandbox image; the store and its
credentials come from the sandbox environment (POND_STORAGE_PATH, POND_CREDS_*),
so there is nothing to configure here.
"""

import subprocess


class PondClient:
    """Search and retrieve archived agent sessions from the shared pond store."""

    def __init__(self, timeout: float = 120.0):
        self.timeout = timeout

    def _run(self, args: list[str]) -> str:
        try:
            result = subprocess.run(
                ["pond", *args],
                capture_output=True,
                text=True,
                timeout=self.timeout,
            )
        except FileNotFoundError:
            raise RuntimeError(
                "pond binary not found; this sandbox image was built without pond"
            ) from None
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"pond {args[0]} timed out after {self.timeout}s") from None
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "no output"
            raise RuntimeError(f"pond {args[0]} failed: {detail}")
        return result.stdout

    def search(
        self,
        query: str,
        limit: int = 10,
        project: str | None = None,
        session_id: str | None = None,
        from_date: str | None = None,
        to_date: str | None = None,
        sort_by: str = "relevance",
    ) -> str:
        """Full-text search over archived sessions; returns readable transcript hits.

        Keep the query semantic (concepts and keywords). Scope with the filters,
        not the query: project (path substring), session_id (exact), from_date /
        to_date (YYYY-MM-DD). sort_by is "relevance" or "recency". Each hit
        carries a message_id and session_id usable with get_message /
        get_session.
        """
        args = ["search", query, "--mode", "fts", "--limit", str(limit), "--sort-by", sort_by]
        if project:
            args += ["--project", project]
        if session_id:
            args += ["--session-id", session_id]
        if from_date:
            args += ["--from-date", from_date]
        if to_date:
            args += ["--to-date", to_date]
        return self._run(args)

    def get_session(
        self,
        session_id: str,
        limit: int = 20,
        from_end: bool = False,
        after_message_id: str | None = None,
    ) -> str:
        """Fetch a whole archived session as a readable transcript.

        from_end=True returns the most recent messages (still chronological).
        Page forward by passing the last returned message id as
        after_message_id.
        """
        args = ["get", "--session-id", session_id, "--session-limit", str(limit)]
        if from_end:
            args += ["--session-from", "end"]
        if after_message_id:
            args += ["--session-after-message-id", after_message_id]
        return self._run(args)

    def get_message(self, message_id: str, context: int = 3) -> str:
        """Fetch one archived message with full detail plus surrounding context.

        Returns the message's complete parts (including tool call and result
        bodies) with `context` conversational neighbors on each side.
        """
        return self._run(
            [
                "get",
                "--message-id",
                message_id,
                "--message-context-before",
                str(context),
                "--message-context-after",
                str(context),
            ]
        )

    def status(self) -> str:
        """Show archive statistics: session/message counts, adapters, last sync."""
        return self._run(["status"])


def _client() -> PondClient:
    return PondClient()
