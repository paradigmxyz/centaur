# Centaur agent plugin

Connect Codex, Claude Code, and other MCP clients to the tools approved for your Centaur principal. Each Centaur deployment has its own MCP URL, normally ending in `/mcp`.

## Running Tools

Centaur serves `/mcp` through Code Mode. It is on by default; operators can turn it off with `apiRs.mcpEnabled: false` (`CENTAUR_MCP_ENABLED=false`). Centaur exposes four stable MCP tools while the connector catalog behind them changes dynamically:

Connector credentials come from the deployment's tool secret declarations: a connector works only if a tool declares the secrets it needs, and the proxy injects them on the sandbox's outbound requests.

```json
{"name": "centaur_catalog_search", "arguments": {"query": "slack messages"}}
{"name": "centaur_catalog_load", "arguments": {"tool": "slack"}}
{"name": "centaur_execute", "arguments": {"code": "..."}}
{"name": "centaur_whoami", "arguments": {}}
```

Search the catalog first; results include each function's signature. Load a connector with `centaur_catalog_load` when the signature is not enough. Then call connectors from one `centaur_execute` cell. `code` is the body of an async function:

```js
const [slack, linear] = await Promise.all([
  tools.slack.channels({ readable_only: true }),
  tools.linear.issues({ team_key: "ENG", limit: 10 }),
]);
console.log(`scanned ${slack.channels.length} Slack channels`);
return { channels: slack.channels.slice(0, 5), issues: linear.issues };
```

Call connectors as `await tools.<connector>.<function>(args)`. Run independent calls together with `Promise.all` or `Promise.allSettled` in the same cell instead of issuing several tool calls. `return` the JSON-serializable value you want back; `console.log` output is returned with the result. A cell has no `fetch`, no module imports, no file access, and no state between calls, and is limited to 120 s, 1000 calls and 8 concurrent calls.

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
