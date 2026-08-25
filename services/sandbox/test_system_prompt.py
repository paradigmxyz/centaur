from __future__ import annotations

import unittest
from pathlib import Path

SYSTEM_PROMPT = Path(__file__).with_name("SYSTEM_PROMPT.md")


class SystemPromptTest(unittest.TestCase):
    def test_company_context_retrieval_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Company-context retrieval]", prompt)
        self.assertIn("use `company_context search` before source-specific tools", prompt)
        self.assertIn("Use hybrid search by default", prompt)
        self.assertIn("Use `--no-hybrid` for exact identifiers", prompt)
        self.assertIn("fewer than two of the top five results", prompt)
        self.assertIn("do not infer conclusions from titles alone", prompt)
        self.assertIn("Distinguish direct internal views from AI-generated research", prompt)
        self.assertIn("If retrieval remains weak", prompt)

    def test_granola_share_links_require_direct_retrieval(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Granola share links]", prompt)
        self.assertIn("pass that exact link to `granola get`", prompt)
        self.assertIn("both `/d/<meeting-uuid>` and `/t/<meeting-uuid>-<share-suffix>`", prompt)
        self.assertIn("Do not substitute a similarly titled meeting", prompt)

    def test_mercator_paid_fallback_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Mercator external service fallback]", prompt)
        self.assertIn("centaur-tools list", prompt)
        self.assertIn("use `mercator search`", prompt)
        self.assertIn("show the exact service, plan, and maximum price", prompt)
        self.assertIn("configured automatic threshold", prompt)
        self.assertIn("above it, wait for explicit user approval", prompt)
        self.assertIn("submit the unchanged handoff", prompt)
        self.assertIn("returns a receipt by default", prompt)
        self.assertIn("wallet is managed by Centaur outside the sandbox", prompt)

    def test_runtime_discovery_and_vlogs_examples_match_available_surfaces(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertNotIn("[Active deployment]", prompt)
        self.assertIn("$CENTAUR_HARNESS_TYPE", prompt)
        self.assertIn("centaur-tools call vlogs thread_logs", prompt)
        self.assertIn("centaur-tools call vlogs thread_trace", prompt)
        self.assertNotIn("|  vlogs thread_logs", prompt)
        self.assertNotIn("|  vlogs thread_trace", prompt)

    def test_model_and_harness_switching_answer_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Model and Harness Switching Answers]", prompt)
        self.assertIn("`--codex`, `--claude` or `--claude-code`, and `--amp`", prompt)
        self.assertIn("`--model <model-id-or-alias>`", prompt)
        self.assertIn("`--model=<model-id-or-alias>`", prompt)
        self.assertIn("`--fable`, `--opus`, `--sonnet`, and `--haiku`", prompt)
        self.assertIn("`--claude --model=fable fix this`", prompt)
        self.assertIn("`--codex --model=gpt-5.2 investigate this`", prompt)
        self.assertIn("`--meta` selects Codex with the Meta provider", prompt)
        self.assertIn("`--bedrock` selects Codex with the Bedrock provider", prompt)
        self.assertIn("`-rsn <effort>` sets Codex reasoning effort", prompt)

    def test_personal_oauth_app_connection_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Personal OAuth app connections]", prompt)
        self.assertIn("centaur-console oauth-apps", prompt)
        self.assertIn("Google, Granola, Attio, Linear, Slack, and GitHub", prompt)
        self.assertIn("Use the returned `start_url`", prompt)
        self.assertIn("Do not invent OAuth links", prompt)
        self.assertIn("validate the connection with `centaur-console permissions`", prompt)
        self.assertIn("look in `oauth_credentials`", prompt)
        self.assertIn("personal `provider_email`", prompt)
        self.assertIn("Centaur can use their personal connected account", prompt)

    def test_scheduled_task_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Scheduled tasks]", prompt)
        self.assertIn("`centaur-console tasks`", prompt)
        self.assertIn(
            "`task`, `create-task`, `update-task`, `delete-task`, or `run-task`", prompt
        )
        self.assertIn(
            "Only create scheduled tasks from MCP or direct-message (DM) sessions",
            prompt,
        )
        self.assertIn("five-field cron expressions in Pacific Time", prompt)
        self.assertIn("Use `dm` as the delivery channel", prompt)
        self.assertIn("encode its recurrence only in the cron expression", prompt)
        self.assertIn("remove cadence phrases", prompt)
        self.assertIn('such as "Each Monday" or "every day at 9"', prompt)
        self.assertIn("Preserve time-window instructions", prompt)
        self.assertIn(
            "Treat the first successful mutation response as authoritative", prompt
        )


if __name__ == "__main__":
    unittest.main()
