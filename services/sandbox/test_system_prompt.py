from __future__ import annotations

import unittest
from pathlib import Path

SYSTEM_PROMPT = Path(__file__).with_name("SYSTEM_PROMPT.md")
OBSERVABILITY_SKILL = (
    Path(__file__).parents[2] / ".agents" / "skills" / "centaur-observability" / "SKILL.md"
)


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

    def test_mpp_fallback_discovery_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("centaur-tools list", prompt)
        self.assertIn('mpp services search "<sanitized task capability>" --limit 5', prompt)
        self.assertIn("mpp services show <service-id>", prompt)
        self.assertIn("Current MPP support discovers candidates only", prompt)

    def test_runtime_discovery_and_vlogs_examples_match_available_surfaces(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()
        skill = OBSERVABILITY_SKILL.read_text()

        self.assertNotIn("[Active deployment]", prompt)
        self.assertIn("$CENTAUR_HARNESS_TYPE", prompt)
        self.assertIn('centaur-tools call vmetrics query \'{"expr":"centaur_deployment_info"}\'', prompt)
        self.assertIn("centaur-tools call vlogs thread_logs", skill)
        self.assertIn("centaur-tools call vlogs thread_trace", skill)

    def test_model_harness_and_persona_switching_answer_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Model, Harness, and Persona Switching Answers]", prompt)
        self.assertIn("`--codex`, `--claude` or `--claude-code`, and `--amp`", prompt)
        self.assertIn("`--model <model-id-or-alias>`", prompt)
        self.assertIn("`--model=<model-id-or-alias>`", prompt)
        self.assertIn("use `--persona <persona-id>` or `--persona=<persona-id>`", prompt)
        self.assertIn("Bare flags such as `--invest` are not persona selectors", prompt)
        self.assertIn("pinned for the lifetime of that thread", prompt)
        self.assertIn("`--fable`, `--opus`, `--sonnet`, and `--haiku`", prompt)
        self.assertIn("`--claude --model=fable fix this`", prompt)
        self.assertIn("`--codex --model=gpt-5.2 investigate this`", prompt)
        self.assertIn("`--meta` selects Codex with the Meta provider", prompt)
        self.assertIn("`--bedrock` selects Codex with the Bedrock provider", prompt)
        self.assertIn("`-rsn <effort>` sets Codex or Claude Code reasoning effort", prompt)

    def test_personal_oauth_app_connection_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Personal OAuth app connections]", prompt)
        self.assertIn("centaur-console oauth-apps", prompt)
        self.assertIn("Google, Granola, Attio, Linear, Slack, GitHub", prompt)
        self.assertIn("Use the returned `start_url`", prompt)
        self.assertIn("Do not invent OAuth links", prompt)
        self.assertIn("confirm with `centaur-console permissions`", prompt)
        self.assertIn("`oauth_credentials` contains the app", prompt)
        self.assertIn("personal `provider_email`", prompt)
        self.assertIn("Do not claim the account is connected until it does", prompt)

    def test_scheduled_task_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Scheduled tasks]", prompt)
        self.assertIn("`centaur-console tasks`", prompt)
        self.assertIn(
            "`task`, `create-task`, `update-task`, `delete-task`, or `run-task`", prompt
        )
        self.assertIn("Only create tasks from MCP or direct-message (DM) sessions", prompt)
        self.assertIn("five-field cron expressions in Pacific Time", prompt)
        self.assertIn("Deliver to `dm`", prompt)
        self.assertIn("Encode recurrence only in the cron expression", prompt)
        self.assertIn('strip cadence phrases like "Each Monday"', prompt)
        self.assertIn("keep time windows that affect the work", prompt)
        self.assertIn("Do not repeat a successful mutation", prompt)

if __name__ == "__main__":
    unittest.main()
