# Centaur agent plugin

Connect Codex, Claude Code, and other MCP clients to the tools approved for your Centaur principal. Each Centaur deployment has its own MCP URL, normally ending in `/mcp`.

## Codex

Add this repository as a marketplace and install the plugin:

```bash
codex plugin marketplace add paradigmxyz/centaur
codex plugin add centaur@centaur
```

Register the deployment as Streamable HTTP and complete OAuth:

```bash
codex mcp add centaur --url <CENTAUR_MCP_URL>
codex mcp login centaur
```

If `centaur` is already registered with the wrong transport, replace it:

```bash
codex mcp remove centaur
codex mcp add centaur --url <CENTAUR_MCP_URL>
codex mcp login centaur
```

Start a new Codex task after installation so it loads the plugin skill.

## Claude Code

Add the marketplace and install the plugin with your deployment URL:

```bash
claude plugin marketplace add paradigmxyz/centaur
claude plugin install centaur@centaur --config mcp_url=<CENTAUR_MCP_URL>
```

Start Claude Code, open `/mcp`, and authenticate `centaur`. The plugin supplies the remote HTTP configuration and Claude stores OAuth credentials outside the plugin.

For local development, validate and load this checkout directly:

```bash
claude plugin validate --strict ./plugins/centaur
claude --plugin-dir ./plugins/centaur
```

## Other MCP clients

Configure a remote Streamable HTTP server using the deployment-specific URL:

```json
{
  "mcpServers": {
    "centaur": {
      "type": "http",
      "url": "<CENTAUR_MCP_URL>"
    }
  }
}
```

Complete OAuth in the client, then verify that `centaur_whoami` returns the expected principal before performing sensitive actions.
