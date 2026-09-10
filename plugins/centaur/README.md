# Centaur agent plugin

Connect Codex, Claude Code, and other MCP clients to the tools approved for your Centaur principal. Each Centaur deployment has its own MCP URL, normally ending in `/mcp`.

## Running Tools

Deployments with `CENTAUR_MCP_V2_ENABLED=true` expose the `centaur` MCP tool. Pass an argv array in `command` to discover and run tools:

```json
{"command": ["list"]}
{"command": ["search", "slack messages"]}
{"command": ["run", "gsuite", "--help"]}
{"command": ["run", "gsuite", "gmail", "--help"]}
{"command": ["run", "slack", "search", "--help"]}
{"command": ["run", "slack", "search", "incident response"]}
```

`centaur run <tool> <argv>` runs the tool's CLI in your principal's sandbox using current Console policy. Start with `centaur run <tool> --help`, then add command segments before `--help` until the relevant options and arguments are shown. Pass each argument as a separate array element. Spaces within values are preserved. Shell expansion, pipes, and redirection are not interpreted.

The result includes `stdout`, `stderr`, `exit_status`, and `timed_out` in both text and structured content. Plain-text help and JSON output are supported. Nonzero exits and timeouts set MCP `isError`. Calls have a 120-second execution timeout. Existing v1 tools with `method` and `arguments` remain available.

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
