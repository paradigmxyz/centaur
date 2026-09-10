---
name: centaur
description: Use an authenticated Centaur MCP deployment to discover and call team-approved tools. Use when a task requires Centaur, its tool catalog, or the user's Centaur identity and permissions.
---

# Centaur

Use Centaur's MCP tools for actions and context exposed by the user's deployment.

## Workflow

1. When identity or authorization matters, call `centaur_whoami` before other Centaur tools.
2. Use the `centaur` MCP tool to list or search the tool catalog when the correct tool is unclear.
3. Before running an unfamiliar CLI command, call `centaur` with `{"command": ["info", "<tool>", "<command>"]}`. Add nested command segments as separate tokens. Treat the returned signature and parameter details as the current contract.
4. Run the command with `{"command": ["run", "<tool>", "<argv>", "..."]}`. Pass every CLI argument as a separate token and do not guess command names or options.
5. Use a legacy per-service MCP tool only when the `centaur` tool cannot complete the request.
6. Summarize consequential writes and return relevant identifiers or links.

Centaur authorizes calls using the signed-in principal's live roles and grants. Never request, paste, print, or store Centaur OAuth tokens.

## Connection recovery

If no Centaur tools are available, explain that the client still needs the deployment-specific MCP endpoint.

- Codex: register it with `codex mcp add centaur --url <CENTAUR_MCP_URL>`, then run `codex mcp login centaur`. The `--url` flag is required for Streamable HTTP and OAuth.
- Claude Code: configure the plugin's `mcp_url`, open `/mcp`, and authenticate the `centaur` server.
- Other MCP clients: configure a remote HTTP server named `centaur` with the deployment's `/mcp` URL and complete its OAuth flow.

Do not substitute a guessed hostname. Ask for the deployment URL when it is not already configured or supplied.
