// The deployment's tool packages mount into a sandbox as CLIs the harness can
// call. Sandbox startup installs a shim on PATH for each allowlisted package
// under TOOLS_PATH, and the `centaur-tools` catalog lists exactly those. A
// tool runs through its shim, its typer CLI, and through the catalog's
// Python-client call that workflows use. Packages baked into the image but
// left off TOOL_ALLOWLIST are not mounted and cannot be run.
//
// e2e/infra/values.yaml allowlists `ssh` (remote-ssh) and `demo`; demo needs
// no credentials, so its output is deterministic.
import { expect, test } from 'bun:test'
import { randomUUID } from 'node:crypto'
import { api, model, slack, turnTimeoutMs } from '../lib'

/** `name=value` lines a command printed, keyed by name. */
function fields(output: string): Record<string, string> {
  return Object.fromEntries([...output.matchAll(/^(\w+)=(.*)$/gm)].map(([, name, value]) => [name, value!.trim()]))
}

test('allowlisted tools mount as CLIs the harness can call, and others do not', async () => {
  const marker = `e2e-${randomUUID().slice(0, 8)}`
  const script = model.runs(
    [
      `echo "catalog=$(centaur-tools list | cut -f1 | sort | paste -sd, -)"`,
      `echo "demo_dir=$(centaur-tools which demo)"`,
      `echo "demo_shim=$(command -v demo)"`,
      `echo "ssh_shim=$(command -v remote-ssh)"`,
      `demo --help > /tmp/demo-help; echo "help_exit=$?"`,
      `echo "help_lists_health=$(grep -c health /tmp/demo-help)"`,
      // The CLI path a harness takes: the shim runs the package's typer app.
      `demo health > /tmp/demo-health; echo "health_exit=$?"`,
      `echo "health=$(python3 -c 'import json; d = json.load(open("/tmp/demo-health")); print(d["ok"], d["details"]["pong"])')"`,
      `remote-ssh --help > /dev/null; echo "ssh_help_exit=$?"`,
      // The Python-client path workflows take: a client method with JSON arguments.
      `echo "call=$(centaur-tools call demo echo '{"message": "${marker}"}')"`,
      `centaur-tools call demo echo '{"msg": "wrong"}' 2> /tmp/bad-call; echo "bad_call_exit=$?"`,
      `echo "bad_call_usage=$(grep -c 'invalid arguments for echo' /tmp/bad-call)"`,
      // Baked into the image under TOOLS_PATH, but not allowlisted.
      `echo "slack_in_image=$(test -f /opt/centaur/tools/productivity/slack/pyproject.toml && echo yes || echo no)"`,
      `command -v slack > /dev/null; echo "slack_shim_exit=$?"`,
      `centaur-tools run slack --help 2> /tmp/slack-run; echo "slack_run_exit=$?"`,
      `echo "slack_run_error=$(cat /tmp/slack-run)"`
    ].join('\n'),
    'Tools checked.'
  )
  const thread = await slack.mention('--codex Check the mounted tools.', script)
  const turn = await thread.nextTurn()
  expect(turn.reply).toBe('Tools checked.')
  expect(turn.execution.status).toBe('completed')

  const output = fields(await model.commandOutput(script))
  expect(output).toMatchObject({
    catalog: 'demo,remote-ssh',
    demo_dir: '/opt/centaur/tools/infra/demo',
    help_exit: '0',
    help_lists_health: expect.stringMatching(/^[1-9]/),
    health_exit: '0',
    health: 'True True',
    ssh_help_exit: '0',
    call: JSON.stringify({ echo: marker }),
    bad_call_exit: '2',
    bad_call_usage: '1',
    slack_in_image: 'yes',
    slack_shim_exit: '1',
    slack_run_exit: '1',
    slack_run_error: 'unknown tool: slack'
  })
  // Shims live in the agent's bin dir and dispatch through the catalog.
  expect(output.demo_shim).toBe('/home/agent/.local/bin/demo')
  expect(output.ssh_shim).toBe('/home/agent/.local/bin/remote-ssh')

  // Free the sandbox's pod for the scenarios still running on the node.
  await api.pause(thread)
}, 2 * turnTimeoutMs)
