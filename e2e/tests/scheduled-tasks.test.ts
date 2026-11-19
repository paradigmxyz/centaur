// Console scheduled tasks, run by api-rs: a due task starts on its schedule,
// runs its prompt as its author on a real harness, posts the answer to Slack,
// and has its next run and outcome recorded in Console; "Run now" starts a
// manual task the same way; and a task whose destination the bot has left is
// refused with the reason, without running.
import { expect, test } from 'bun:test'
import {
  CHANNEL,
  DEFAULTS_CHANNEL,
  USER,
  api,
  eventually,
  model,
  releaseSandboxesAfterAll,
  scheduledTasks,
  slack,
  turnTimeoutMs,
  type Channel,
  type Script
} from '../lib'

releaseSandboxesAfterAll()

const FOOTER = `Sent by <@${USER.id}>'s scheduled task`

async function waitForRun(runId: string): Promise<any> {
  return eventually(`workflow run ${runId} to complete`, turnTimeoutMs, async () => {
    const { run } = await api.ok('GET', `/api/workflows/runs/${runId}`)
    if (run.status === 'completed') return run
    if (['failed', 'cancelled'].includes(run.status)) {
      throw new Error(`workflow run ${runId} ended ${run.status}: ${JSON.stringify(run)}`)
    }
    return undefined
  })
}

/** The bot's top-level post in `channel` carrying the script's answer. */
async function delivered(channel: Channel, answer: Script) {
  return eventually(`the answer to reach #${channel.name}`, 30_000, async () =>
    (await slack.history(channel)).find(message => message.text?.includes(answer.text))
  )
}

test.concurrent('a due task runs on its schedule and posts its answer to Slack', async () => {
  const answer = model.says('Scheduled digest: nothing is on fire.')
  await model.register(answer)
  const task = await scheduledTasks.create({
    prompt: `Summarize open incidents. ${answer.token}`,
    channel: CHANNEL,
    cron: '* * * * *'
  })
  // Console computes the first run when the task is saved.
  const firstRun = task.nextRunAt
  expect(firstRun).not.toBeNull()

  const ran = await eventually('api-rs to start the scheduled run', 150_000, async () => {
    const current = await scheduledTasks.get(task.id)
    return current.lastRunId ? current : undefined
  })
  await scheduledTasks.disable(task)

  expect(ran.lastError).toBeNull()
  expect(ran.lastEnqueuedAt).toEqual(firstRun)
  // api-rs advanced the schedule to the following minute.
  expect(ran.nextRunAt!.getTime() - firstRun!.getTime()).toBe(60_000)

  const run = await waitForRun(ran.lastRunId!)
  expect(run.workflow_name).toBe('console_workflow')
  const message = await delivered(CHANNEL, answer)
  expect(message.text).toContain(FOOTER)
  expect(message.user).not.toBe(USER.id)
  const [request] = await model.requests(answer)
  expect(request).toBeDefined()
}, turnTimeoutMs + 180_000)

test.concurrent('Run now starts a manual task without scheduling it', async () => {
  const answer = model.says('On-demand digest: all quiet.')
  await model.register(answer)
  const task = await scheduledTasks.create({
    prompt: `Summarize open incidents now. ${answer.token}`,
    channel: CHANNEL,
    cron: null
  })
  expect(task.nextRunAt).toBeNull()

  const run = await scheduledTasks.runNow(task)
  if (!run.started) throw new Error(`Run now was refused: ${run.status} ${run.message}`)

  const after = await scheduledTasks.get(task.id)
  expect(after.lastRunId).toBe(run.runId)
  expect(after.lastRunAt).not.toBeNull()
  expect(after.lastError).toBeNull()
  expect(after.nextRunAt).toBeNull()
  expect(after.lastEnqueuedAt).toBeNull()

  await waitForRun(run.runId)
  expect((await delivered(CHANNEL, answer)).text).toContain(FOOTER)
}, turnTimeoutMs + 60_000)

test.concurrent('a task whose channel the bot left is refused with the reason', async () => {
  const task = await scheduledTasks.create({
    prompt: 'This task never runs.',
    channel: DEFAULTS_CHANNEL,
    cron: null
  })
  await scheduledTasks.botLeaves(DEFAULTS_CHANNEL)

  const reason = 'Slack delivery destination is no longer available to the task owner'
  expect(await scheduledTasks.runNow(task)).toEqual({ started: false, status: 422, message: reason })
  const after = await scheduledTasks.get(task.id)
  expect(after.lastError).toBe(reason)
  expect(after.lastRunId).toBeNull()
}, 60_000)
