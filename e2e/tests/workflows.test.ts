// Durable workflows on the real control plane: discovery of workflow files,
// run idempotency and identity, cancellation when a workflow disappears, the
// Python host's durable context methods, including an agent turn on a real
// harness, and Slack buttons a workflow posts, clicked in Slack. Workflow files are written into the api-rs pod's workflow
// directory, which is what WORKFLOW_DIRS points at on this stack.
import { randomUUID } from 'node:crypto'
import { expect, test } from 'bun:test'
import {
  CHANNEL,
  USER,
  USER_B,
  api,
  cluster,
  eventually,
  model,
  providerCredentials,
  slack,
  workflows
} from '../lib'

const WORKFLOW_DIR = '/app/workflows'

/** Writes a workflow file and waits until api-rs has scheduled it. */
async function addWorkflow(name: string, source: string): Promise<void> {
  await cluster.exec(`cat > ${WORKFLOW_DIR}/${name}.py`, source)
  await waitForSchedule(name, true)
}

async function waitForSchedule(name: string, present: boolean): Promise<void> {
  await eventually(`workflow ${name} to be ${present ? 'scheduled' : 'unscheduled'}`, 30_000, async () => {
    const { schedules } = await api.ok('GET', '/api/workflows/schedules')
    const scheduled = schedules.some((schedule: any) => schedule.schedule_id === name)
    return scheduled === present ? true : undefined
  })
}

async function startRun(name: string, input: unknown, idempotencyKey = `${name}-${randomUUID()}`) {
  return api.ok('POST', '/api/workflows/runs', {
    workflow_name: name,
    input,
    idempotency_key: idempotencyKey,
    harness_type: 'codex',
    max_attempts: 1
  })
}

async function waitForRun(runId: string, status: string, timeoutMs = 30_000): Promise<any> {
  return eventually(`workflow run ${runId} to be ${status}`, timeoutMs, async () => {
    const { run } = await api.ok('GET', `/api/workflows/runs/${runId}`)
    if (run.status === status) return run
    if (['completed', 'failed', 'cancelled'].includes(run.status)) {
      throw new Error(`workflow run ${runId} ended ${run.status}, expected ${status}: ${JSON.stringify(run)}`)
    }
    return undefined
  })
}

/** A scheduled workflow that echoes its input, optionally sleeping or failing. */
function echoWorkflow(name: string): string {
  return `
import asyncio
from pathlib import Path

WORKFLOW_NAME = "${name}"
SCHEDULE = {
    "schedule_id": "${name}",
    "interval_seconds": 3600,
    "enabled": True,
    "no_delivery": True,
    "input": {"source": "centaur-e2e"},
}


async def handler(params, ctx):
    started_path = params.get("started_path")
    if started_path:
        Path(started_path).touch()
    sleep_ms = int(params.get("sleep_ms") or 0)
    if sleep_ms:
        await asyncio.sleep(sleep_ms / 1000)
    if params.get("always_fail"):
        raise RuntimeError("simulated workflow failure")
    return {"workflow_name": ctx.workflow_name, "received": params}
`
}

const uniqueName = (kind: string) => `e2e_${kind}_${randomUUID().replaceAll('-', '')}`

test('an added workflow is discovered and runs', async () => {
  const name = uniqueName('added')
  await addWorkflow(name, echoWorkflow(name))

  const started = await startRun(name, { case: 'added' })
  expect(started.created).toBe(true)
  const run = await waitForRun(started.run_id, 'completed')
  expect(run.result.output).toEqual({ workflow_name: name, received: { case: 'added' } })
}, 120_000)

test('concurrent starts with one idempotency key share one run', async () => {
  const name = uniqueName('idempotent')
  await addWorkflow(name, echoWorkflow(name))

  const key = `${name}-concurrent`
  const [first, second] = await Promise.all([
    startRun(name, { case: 'concurrent' }, key),
    startRun(name, { case: 'concurrent' }, key)
  ])
  for (const field of ['run_id', 'initial_run_id', 'task_id']) {
    expect({ field, second: second[field] }).toEqual({ field, second: first[field] })
  }
  expect([first.created, second.created].sort()).toEqual([false, true])
  expect(first.initial_run_id).toBe(first.run_id)
}, 120_000)

test('replaying a failed run keeps its initial identity', async () => {
  const name = uniqueName('replay')
  await addWorkflow(name, echoWorkflow(name))

  const request = {
    workflow_name: name,
    input: { always_fail: true },
    idempotency_key: `${name}-retrying`,
    max_attempts: 2
  }
  const original = await api.ok('POST', '/api/workflows/runs', request)
  expect(original.run_id).toBe(original.initial_run_id)
  await waitForRun(original.initial_run_id, 'failed')

  const replayed = await api.ok('POST', '/api/workflows/runs', request)
  expect(replayed.created).toBe(false)
  expect(replayed.task_id).toBe(original.task_id)
  expect(replayed.initial_run_id).toBe(original.initial_run_id)
  expect(replayed.run_id).not.toBe(original.run_id)
  const { run } = await api.ok('GET', `/api/workflows/runs/${original.initial_run_id}`)
  expect(run.initial_run_id).toBe(original.initial_run_id)
}, 120_000)

test('removing a workflow cancels its running run', async () => {
  const name = uniqueName('removed')
  await addWorkflow(name, echoWorkflow(name))

  const startedPath = `/tmp/${name}.started`
  const started = await startRun(name, { sleep_ms: 60_000, started_path: startedPath })
  // The run is marked running before the host loads the module; wait for the
  // handler itself so removing the file cannot race loading it.
  await eventually('the workflow handler to start', 30_000, async () =>
    (await cluster.exec(`test -f ${startedPath} && echo started || true`)) === 'started' ? true : undefined
  )
  await cluster.exec(`rm ${WORKFLOW_DIR}/${name}.py`)
  await waitForSchedule(name, false)
  await waitForRun(started.run_id, 'cancelled')
}, 120_000)

test('durable context methods survive host restarts and reach a real harness', async () => {
  const name = uniqueName('durable')
  const child = uniqueName('child')
  const correlationId = `${name}-event`
  const answer = model.says('Workflow agent answer.')
  await model.register(answer)
  await cluster.exec(`cat > ${WORKFLOW_DIR}/${child}.py`, `
WORKFLOW_NAME = "${child}"


async def handler(params, ctx):
    return {"workflow_name": ctx.workflow_name, "received": params}
`)
  await addWorkflow(name, `
import uuid

WORKFLOW_NAME = "${name}"
HOST_INSTANCE_ID = uuid.uuid4().hex
SCHEDULE = {
    "schedule_id": "${name}",
    "interval_seconds": 3600,
    "enabled": True,
    "no_delivery": True,
    "input": {"source": "centaur-e2e"},
}


async def handler(params, ctx):
    checkpoint = await ctx.step(
        "checkpoint_before_sleep",
        lambda: {"host_instance_id": HOST_INSTANCE_ID},
    )
    await ctx.sleep("durable_sleep", 0.05)
    event = await ctx.wait_for_event(
        "durable_event", "e2e_test", params["correlation_id"], timeout=30,
    )
    agent = await ctx.agent_turn(
        params["prompt"], model=params["model"], idle_timeout_ms=60_000, max_duration_ms=180_000,
    )
    child = await ctx.start_workflow(
        params["child_workflow_name"], {"from_parent": True}, idempotency_key=f"{ctx.run_id}:child",
    )
    return {
        "checkpoint": checkpoint,
        "result_host_instance_id": HOST_INSTANCE_ID,
        "event": event,
        "agent": agent,
        "child": child,
    }
`)

  const started = await startRun(name, {
    child_workflow_name: child,
    correlation_id: correlationId,
    model: 'gpt-5.4',
    prompt: `Answer the workflow. ${answer.token}`
  })
  await api.ok('POST', '/api/workflows/events', {
    event_type: 'e2e_test',
    correlation_id: correlationId,
    payload: { approved: true }
  })
  const run = await waitForRun(started.run_id, 'completed', 300_000)
  const output = run.result.output

  // The step ran before the durable sleep, in a host process the sleep ended:
  // its result is replayed from the checkpoint, not recomputed.
  expect(output.checkpoint.host_instance_id).not.toBe(output.result_host_instance_id)
  expect(output.event).toEqual({ approved: true })
  expect(output.agent.status).toBe('completed')
  expect(output.agent.result_text).toBe(answer.text)
  const [request] = await model.requests(answer)
  expect(request?.model).toBe('gpt-5.4')
  expect(request?.credential).toBe(providerCredentials.codex)

  expect(output.child.created).toBe(true)
  const childRun = await waitForRun(output.child.run_id, 'completed')
  expect(childRun.result.output.received).toEqual({ from_parent: true })
}, 400_000)

/** A workflow that posts what its input asks for to Slack and returns the posted message. */
function slackPoster(name: string, post: string): string {
  return `
WORKFLOW_NAME = "${name}"
SCHEDULE = {"schedule_id": "${name}", "interval_seconds": 3600, "enabled": False, "input": {}}


async def handler(params, ctx):
${post}
`
}

async function runsOf(name: string): Promise<any[]> {
  const { runs } = await api.ok('GET', `/api/workflows/runs?workflow_name=${name}`)
  return runs
}

test('a workflow button click starts its target once, for the person who clicked', async () => {
  const target = uniqueName('target')
  const asker = uniqueName('asker')
  await addWorkflow(target, slackPoster(target, '    return params'))
  await addWorkflow(asker, slackPoster(asker, `    return await ctx.slack_buttons(
        "ask", channel=params["channel"], text="Ship release-42?", workflow="${target}",
        input={"release": "release-42"}, buttons={"approve": "Approve"},
    )`))
  const asked = await startRun(asker, { channel: CHANNEL.id })
  const posted = (await waitForRun(asked.run_id, 'completed')).result.output
  const message = await slack.message(CHANNEL, posted.ts)
  const approve = message.blocks
    ?.flatMap(block => block.elements ?? [])
    .find(element => element.action_id?.endsWith(':approve'))?.action_id
  if (!approve) throw new Error(`the posted message has no approve button: ${JSON.stringify(message)}`)

  const click = await slack.click(CHANNEL, posted.ts, approve, { as: USER_B })
  expect(click.status).toBe(200)
  const started = await eventually(`a ${target} run`, 30_000, async () => (await runsOf(target))[0])
  const run = await waitForRun(started.run_id, 'completed')
  expect(run.result.output).toMatchObject({
    release: 'release-42',
    click: { action: 'approve', user_id: USER_B.id, channel_id: CHANNEL.id, message_ts: posted.ts }
  })

  // slackbotv2 acknowledges a click only after api-rs accepts it, so once a
  // redelivery is acknowledged, any run it would start already exists.
  expect((await slack.click(CHANNEL, posted.ts, approve, { as: USER_B, actionTs: click.actionTs })).status).toBe(200)
  expect(await runsOf(target)).toHaveLength(1)

  // A click carrying a value the workflow did not sign is refused, and the
  // clicker is told so.
  expect((await slack.click(CHANNEL, posted.ts, approve, { value: 'v1.forged.signature' })).status).toBe(200)
  await eventually('the refusal to reach the clicker', 30_000, async () =>
    (await slack.ephemeral(USER)).some(ephemeral => ephemeral.text === 'This request is no longer available.')
      ? true
      : undefined
  )
  expect(await runsOf(target)).toHaveLength(1)
}, 120_000)

test('other Slack block actions reach workflows as events, without their response URL', async () => {
  const poster = uniqueName('poster')
  const actionId = `e2e.${randomUUID()}`
  await addWorkflow(poster, slackPoster(poster, `    return await ctx.post_to_slack(params["channel"], "Pick one", blocks=[
        {"type": "actions", "block_id": "e2e-pick", "elements": [
            {"type": "button", "action_id": "${actionId}", "value": "picked",
             "text": {"type": "plain_text", "text": "Pick"}}
        ]}
    ])`))
  const asked = await startRun(poster, { channel: CHANNEL.id })
  const posted = (await waitForRun(asked.run_id, 'completed')).result.output

  expect((await slack.click(CHANNEL, posted.ts, actionId, { as: USER_B })).status).toBe(200)
  const event = await eventually('the block action event', 30_000, () =>
    workflows.event(`slack.block_action.${actionId}`)
  )
  expect(event).toMatchObject({
    action_id: actionId,
    value: 'picked',
    user_id: USER_B.id,
    channel_id: CHANNEL.id,
    message_ts: posted.ts
  })
  expect(JSON.stringify(event)).not.toContain('response_url')
  expect(JSON.stringify(event)).not.toContain('e2e-response-token')
}, 120_000)
