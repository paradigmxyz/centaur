// A Slack thread can run on the nanocode harness, whose turns execute in the
// long-lived centaur-brain service instead of a sandbox. The scripted answer
// reaches Slack, the turn's completion and first-token latency come from the
// session event stream api-rs proxies from the brain, and nothing is recorded
// in session_executions or session.sandbox_ready, so no Kubernetes Sandbox
// exists for the thread. The timing scenarios report time to first token for
// nanocode next to a sandboxed harness, without asserting a threshold.
import { expect, test } from 'bun:test'
import {
  db,
  eventually,
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
