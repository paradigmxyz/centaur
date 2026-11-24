// A Slack thread can run on the nanocode harness, whose turns execute in the
// long-lived centaur-brain service instead of a sandbox: the scripted answer
// reaches Slack, the execution completes, and the control plane records a
// `brain-` sandbox handle with no Kubernetes objects behind it. The timing
// scenarios report time to first token for nanocode next to the sandboxed
// harnesses, without asserting a threshold.
import { expect, test } from 'bun:test'
import {
  cluster,
  eventually,
  firstTokenTiming,
  model,
  releaseSandboxesAfterAll,
  slack,
  turnTimeoutMs
} from '../lib'

releaseSandboxesAfterAll()

test.concurrent('nanocode answers a Slack mention from the brain, without a sandbox', async () => {
  const answer = model.says('Hello from the brain.')

  const thread = await slack.mention('--nanocode Say hello.', answer)
  const turn = await thread.nextTurn()

  expect(turn.reply).toBe(answer.text)
  expect(turn.execution.status).toBe('completed')
  expect(turn.sandbox.harness).toBe('nanocode')
  // The brain backend names its logical sandbox handles `brain-` and creates
  // no Kubernetes resources for them.
  expect(turn.sandbox.id.startsWith('brain-')).toBe(true)
  expect(await cluster.sandboxResources(turn.sandbox.id)).toEqual([])
}, turnTimeoutMs)

async function firstTokenMs(executionId: string): Promise<number> {
  const timing = await eventually(
    `the first-token marker of execution ${executionId}`,
    turnTimeoutMs,
    async () => firstTokenTiming(executionId)
  )
  return timing.firstTokenAt.getTime() - timing.startedAt.getTime()
}

// Time to first token per harness, on the same stack and scripted model: the
// first turn of a thread (a sandboxed harness boots its sandbox; this stack
// runs without a warm pool) and a second turn in that thread. Reported, not
// asserted.
for (const harness of ['codex', 'nanocode']) {
  test.concurrent(`${harness} reports its time to first token`, async () => {
    const thread = await slack.mention(`--${harness} Say hello.`, model.says('First.'))
    const first = await thread.nextTurn()
    await thread.mention('Again.', model.says('Second.'))
    const second = await thread.nextTurn()

    expect(first.execution.status).toBe('completed')
    expect(second.execution.status).toBe('completed')
    const firstMs = await firstTokenMs(first.execution.execution_id)
    const secondMs = await firstTokenMs(second.execution.execution_id)
    console.log(`time to first token: ${harness} first turn ${firstMs}ms, second turn ${secondMs}ms`)
    expect(firstMs).toBeGreaterThanOrEqual(0)
  }, turnTimeoutMs * 2)
}
