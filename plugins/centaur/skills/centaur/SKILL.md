---
name: centaur
description: Use an authenticated Centaur MCP deployment to discover and call team-approved tools. Use when a task requires Centaur, its tool catalog, or the user's Centaur identity and permissions.
---

# Centaur

Use Centaur's MCP tools for actions and context exposed by the user's deployment.

## Workflow

1. When identity or authorization matters, call `centaur_whoami` before other Centaur tools.
2. Choose the narrowest Centaur tool that satisfies the request.
3. Each tool package accepts a `method` and an `arguments` object. If its methods or parameters are unclear, call that tool with `method: "help"` first.
4. Treat the MCP tool description and help result as the current contract. Do not guess method names or parameters.
5. Summarize consequential writes and return relevant identifiers or links.

Centaur authorizes calls using the signed-in principal's live roles and grants. Never request, paste, print, or store Centaur OAuth tokens.

## Connection recovery

If no Centaur tools are available, explain that the client still needs the deployment-specific MCP endpoint.

- Codex: register it with `codex mcp add centaur --url <CENTAUR_MCP_URL>`, then run `codex mcp login centaur`. The `--url` flag is required for Streamable HTTP and OAuth.
- Claude Code: configure the plugin's `mcp_url`, open `/mcp`, and authenticate the `centaur` server.
- Other MCP clients: configure a remote HTTP server named `centaur` with the deployment's `/mcp` URL and complete its OAuth flow.

Do not substitute a guessed hostname. Ask for the deployment URL when it is not already configured or supplied.
