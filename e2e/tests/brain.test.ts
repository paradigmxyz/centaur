// A Slack thread can run on the nanocode harness, whose turns execute in the
// long-lived centaur-brain service instead of a sandbox. The scripted answer
// reaches Slack, the turn's completion and first-token latency come from the
// session event stream api-rs proxies from the brain, and nothing is recorded
// in session_executions or session.sandbox_ready, so no Kubernetes Sandbox
// exists for the thread. The timing scenarios report time to first token for
// nanocode next to a sandboxed harness, without asserting a threshold. A
// nanocode turn's Code Mode cell runs in the shared executor, and its
// requests go through the shared tenancy iron-proxy with the channel
// principal's credentials; none of it needs a sandbox.
import { expect, test } from 'bun:test'
import {
  CHANNEL,
  db,
  eventually,
  ironControl,
  model,
  releaseSandboxesAfterAll,
  slack,
  turnTimeoutMs,
  type Execution,
  type Thread
} from '../lib'

releaseSandboxesAfterAll()

test.concurrent('nanocode answers a Slack mention from the brain, without a sandbox', async () => {
  const answer = model.says('Hello from the brain.')

  const thread = await slack.mention('--nanocode Say hello.', answer)
  const turn = await thread.nextBrainTurn()

  expect(turn.reply).toBe(answer.text)
  expect(turn.execution.status).toBe('completed')
  expect(turn.firstTokenMs).toBeGreaterThanOrEqual(0)
  // Nanocode turns leave no sandbox_ready event, which is where a thread's
  // sandbox handle would come from; sandboxes carry no thread label, so the
  // durable rows are what proves the thread has no sandbox at all.
  expect(await thread.sandboxes()).toEqual([])
}, turnTimeoutMs)

test.concurrent('nanocode runs Code Mode cells on the executor as the channel principal', async () => {
  // `context.sandbox` is the channel principal's Console permissions: the
  // executor fetched them through the tenancy iron-proxy with the turn token,
  // and iron-proxy added the api-rs credential proxy-sync serves for that
  // principal, which api-rs accepted without a proxy row. The turn token
  // carries the deployment's tool filter (TOOL_ALLOWLIST=ssh,demo in
  // e2e/infra/values.yaml), which names no connector, so the cell sees none.
  const cell = model.runsCell(`
    return {
      thread: context.thread_key,
      harness: context.harness,
      principal: context.sandbox?.principal_id ?? null,
      error: context.sandbox_error ?? null,
      connectors: Object.keys(tools),
    }`, 'Checked with Code Mode.')

  const thread = await slack.mention('--nanocode What can this channel do?', cell)
  const turn = await thread.nextBrainTurn()

  expect(turn.reply).toBe(cell.text)
  expect(turn.execution.status).toBe('completed')
  // Exactly this channel's principal: the session's, and the one api-rs
  // answered for with the credential iron-proxy injected.
  const principal = await ironControl.slackChannelPrincipalId(CHANNEL.id)
  const [session] = await db`
    select iron_control_principal as principal from sessions where thread_key = ${thread.key}`
  expect(session?.principal).toBe(principal)
  const output = JSON.parse(await model.toolOutput(cell))
  expect(output).toEqual({
    thread: thread.key,
    harness: 'codemode',
    principal,
    error: null,
    connectors: []
  })
  expect(await thread.sandboxes()).toEqual([])
}, turnTimeoutMs)

/**
 * Time from execution start to first token for a sandboxed harness, from the
 * `latency_ms` api-rs stamped in the durable session.first_token row.
 */
async function sandboxFirstTokenMs(executionId: string): Promise<number> {
  const timing = await eventually(`the first-token marker of execution ${executionId}`, turnTimeoutMs, async () => {
    const [row] = await db`
      select (payload->>'latency_ms')::int as "latencyMs"
      from session_events
      where execution_id = ${executionId} and event_type = 'session.first_token'`
    return row ?? undefined
  })
  return timing.latencyMs as number
}

/** The next turn of a thread, with its time to first token, whichever harness ran it. */
async function nextTimedTurn(thread: Thread, harness: string): Promise<{ execution: Execution; firstTokenMs: number }> {
  if (harness === 'nanocode') return thread.nextBrainTurn()
  const turn = await thread.nextTurn()
  return { execution: turn.execution, firstTokenMs: await sandboxFirstTokenMs(turn.execution.execution_id) }
}

// Time to first token per harness, on the same stack and scripted model: the
// first turn of a thread (a sandboxed harness boots its sandbox; this stack
// runs without a warm pool) and a second turn in that thread. Reported, not
// asserted.
for (const harness of ['codex', 'nanocode']) {
  test.concurrent(`${harness} reports its time to first token`, async () => {
    const thread = await slack.mention(`--${harness} Say hello.`, model.says('First.'))
    const first = await nextTimedTurn(thread, harness)
    await thread.mention('Again.', model.says('Second.'))
    const second = await nextTimedTurn(thread, harness)

    expect(first.execution.status).toBe('completed')
    expect(second.execution.status).toBe('completed')
    console.log(`time to first token: ${harness} first turn ${first.firstTokenMs}ms, second turn ${second.firstTokenMs}ms`)
    expect(first.firstTokenMs).toBeGreaterThanOrEqual(0)
  }, turnTimeoutMs * 2)
}
