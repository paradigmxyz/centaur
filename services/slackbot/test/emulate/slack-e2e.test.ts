import { createHmac } from 'node:crypto'
import { connect } from 'node:net'
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'bun:test'
import { createEmulator, type Emulator } from 'emulate'
import { createSlackWebClient } from '../../src/slack/installations'
import { pollFinalDeliveriesOnce } from '../../src/centaur/final-delivery'
import { createPatchedSlackApi } from './slack-patches'

const IMPLEMENTATION = 'custom-web-api-wrapper'
const BOT_TOKEN = 'xoxb-centaur-emulate'
const USER_TOKEN = 'xoxp-centaur-user'
const API_KEY = 'aiv2_slackbot_emulate'
const SIGNING_SECRET = 'emulate-signing-secret'
const BOT_USER_ID = 'U000000001'
const USER_ID = 'UEMULATEUSER'
const TEAM_ID = 'T000000001'
const CHANNEL_ID = 'C000000001'

type WorkflowRunRequest = {
  workflow_name: string
  trigger_key: string
  input: {
    thread_key: string
    parts: Array<{ type: string; text?: string }>
    history_messages?: Array<{ role: string; parts: Array<{ type: string; text?: string }> }>
    message_id: string
    user_id: string
    metadata: { is_mention?: boolean; slack?: Record<string, unknown> }
    delivery: {
      platform: string
      channel: string
      thread_ts: string
      recipient_user_id: string
      recipient_team_id: string
    }
  }
}

type FakeDelivery = {
  execution_id: string
  thread_key: string
  delivery: Record<string, unknown>
  final_payload: Record<string, unknown>
}

let emulator: Emulator
let patchedSlack: Awaited<ReturnType<typeof createPatchedSlackApi>>
let centaur: Awaited<ReturnType<typeof createFakeCentaur>>
let app: Awaited<typeof import('../../src/index')>['app']
let slack: ReturnType<typeof createSlackWebClient>
let botSlack: ReturnType<typeof createSlackWebClient>

beforeAll(async () => {
  const slackPort = await preferredPort(4003)
  emulator = await createEmulator({
    service: 'slack',
    port: slackPort,
    seed: {
      tokens: {
        [BOT_TOKEN]: {
          login: BOT_USER_ID,
          scopes: ['chat:write', 'channels:read', 'users:read', 'reactions:write']
        },
        [USER_TOKEN]: {
          login: USER_ID,
          scopes: ['chat:write', 'channels:read', 'users:read', 'reactions:write']
        }
      },
      slack: {
        team: { name: 'Centaur E2E', domain: 'centaur-e2e' },
        users: [{ name: 'tester', real_name: 'Test User', email: 'tester@example.com' }],
        channels: [{ name: 'centaur-e2e', topic: 'Slackbot E2E tests' }],
        bots: [{ name: 'centaur' }],
        signing_secret: SIGNING_SECRET
      }
    }
  })
  patchedSlack = await createPatchedSlackApi(emulator)
  centaur = await createFakeCentaur()
  const slackApiUrl = `${patchedSlack.url}/api/`
  slack = createSlackWebClient(USER_TOKEN, { slackApiUrl })
  botSlack = createSlackWebClient(BOT_TOKEN, { slackApiUrl })

  Object.assign(process.env, {
    NODE_ENV: 'test',
    SLACK_BOT_TOKEN: BOT_TOKEN,
    SLACK_API_URL: slackApiUrl,
    SLACK_SIGNING_SECRET: SIGNING_SECRET,
    SLACKBOT_API_KEY: API_KEY,
    CENTAUR_API_URL: centaur.url,
    SLACK_EVENT_DEDUP_TTL_MS: '600000',
    SLACKBOT_TRIGGER_BOT_ALLOWLIST: 'app:AALERTMANAGER',
    RUNTIME_ERROR_ALERT_CHANNEL: ''
  })

  ;({ app } = await import('../../src/index'))
})

beforeEach(async () => {
  emulator.reset()
  centaur.reset()
})

afterAll(async () => {
  await patchedSlack?.close()
  await emulator?.close()
  await centaur?.close()
})

describe(`Slack Emulate E2E (${IMPLEMENTATION})`, () => {
  it('dispatches an app_mention into a slack_thread_turn workflow with Slack metadata', async () => {
    const parent = await postUserMessage(`<@${BOT_USER_ID}> summarize this incident`)
    const waits: Promise<unknown>[] = []
    const response = await app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-emulate-mention',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          ts: parent.ts,
          text: `<@${BOT_USER_ID}> summarize this incident`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    expect(await response.json()).toEqual({ ok: true })
    await Promise.all(waits)

    const run = onlyRun()
    expect(run.workflow_name).toBe('slack_thread_turn')
    expect('eager_start' in run).toBe(false)
    expect(run.trigger_key).toBe(`slack:${TEAM_ID}:${CHANNEL_ID}:${parent.ts}`)
    expect(run.input.thread_key).toBe(`slack:${TEAM_ID}:${CHANNEL_ID}:${parent.ts}`)
    expect(run.input.message_id).toBe(`slack:${TEAM_ID}:${CHANNEL_ID}:${parent.ts}`)
    expect(run.input.parts).toEqual([{ type: 'text', text: 'summarize this incident' }])
    expect(run.input.user_id).toBe(USER_ID)
    expect(run.input.metadata.is_mention).toBe(true)
    expect(run.input.metadata.slack?.message_ts).toBe(parent.ts)
    expect(run.input.delivery).toMatchObject({
      platform: 'slack',
      channel: CHANNEL_ID,
      thread_ts: parent.ts,
      recipient_user_id: USER_ID,
      recipient_team_id: TEAM_ID
    })
  })

  it('includes prior Slack thread replies as history for reply mentions', async () => {
    const parent = await postUserMessage('Original request')
    await postBotMessage('Earlier assistant context', parent.ts)
    await postUserMessage('Prior user clarification', parent.ts)
    const current = await postUserMessage(`<@${BOT_USER_ID}> retry`, parent.ts)
    const waits: Promise<unknown>[] = []

    await app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-emulate-thread-reply',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          ts: current.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> retry`
        }
      }),
      {},
      waitUntilContext(waits)
    )
    await Promise.all(waits)

    const history = onlyRun().input.history_messages ?? []
    expect(history.map(item => item.role)).toEqual(['user', 'assistant', 'user'])
    expect(history.flatMap(item => item.parts.map(part => part.text))).toContain('Original request')
    expect(history.flatMap(item => item.parts.map(part => part.text))).toContain(
      'Earlier assistant context'
    )
    expect(history.flatMap(item => item.parts.map(part => part.text))).toContain(
      'Prior user clarification'
    )
  })

  it('dispatches an Alertmanager-style bot-authored mention into a Slack workflow', async () => {
    const waits: Promise<unknown>[] = []
    const response = await app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-emulate-alertmanager-bot',
        event: {
          type: 'message',
          subtype: 'bot_message',
          bot_id: 'BALERTMANAGER',
          app_id: 'AALERTMANAGER',
          bot_profile: {
            user_id: 'UALERTMANAGER',
            app_id: 'AALERTMANAGER',
            name: 'Alertmanager'
          },
          channel: CHANNEL_ID,
          ts: '1779620985.044779',
          text: `<@${BOT_USER_ID}>`,
          attachments: [
            {
              title: 'ValidatorConsensusFailure',
              text: 'consensus test is failing on prd-nae',
              fields: [
                { title: 'cluster', value: 'prd-nae' },
                { title: 'severity', value: 'critical' }
              ]
            }
          ]
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

    const run = onlyRun()
    expect(run.workflow_name).toBe('slack_thread_turn')
    expect(run.trigger_key).toBe(`slack:${TEAM_ID}:${CHANNEL_ID}:1779620985.044779`)
    expect(run.input.parts).toEqual([
      {
        type: 'text',
        text: [
          'ValidatorConsensusFailure',
          'consensus test is failing on prd-nae',
          'cluster: prd-nae',
          'severity: critical'
        ].join('\n')
      }
    ])
    expect(run.input.user_id).toBe('UALERTMANAGER')
    expect(run.input.metadata.is_mention).toBe(true)
    expect(run.input.metadata.slack?.bot_id).toBe('BALERTMANAGER')
    expect(run.input.metadata.slack?.app_id).toBe('AALERTMANAGER')
    expect(run.input.delivery).toMatchObject({
      platform: 'slack',
      channel: CHANNEL_ID,
      thread_ts: '1779620985.044779',
      recipient_user_id: 'UALERTMANAGER',
      recipient_team_id: TEAM_ID
    })
  })

  it('ignores self bot-originated events and duplicate Slack event IDs', async () => {
    const botMessage = await postBotMessage(`<@${BOT_USER_ID}> bot echo`)
    const waits: Promise<unknown>[] = []
    await app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-emulate-bot',
        event: {
          type: 'app_mention',
          bot_id: 'BEMULATE',
          user: BOT_USER_ID,
          channel: CHANNEL_ID,
          ts: botMessage.ts,
          text: `<@${BOT_USER_ID}> bot echo`
        }
      }),
      {},
      waitUntilContext(waits)
    )
    await Promise.all(waits)
    expect(centaur.workflowRuns).toHaveLength(0)

    const message = await postUserMessage(`<@${BOT_USER_ID}> once`)
    const payload = signedSlackEvent({
      event_id: 'Ev-emulate-duplicate',
      event: {
        type: 'app_mention',
        user: USER_ID,
        channel: CHANNEL_ID,
        ts: message.ts,
        text: `<@${BOT_USER_ID}> once`
      }
    })
    const firstWaits: Promise<unknown>[] = []
    const first = await app.request(
      '/api/webhooks/slack',
      payload,
      {},
      waitUntilContext(firstWaits)
    )
    await Promise.all(firstWaits)
    const second = await app.request('/api/webhooks/slack', payload)

    expect(first.status).toBe(200)
    expect(second.status).toBe(200)
    expect(await second.json()).toEqual({ ok: true, duplicate: true })
    expect(centaur.workflowRuns).toHaveLength(1)
  })

  it('records reactions in Emulate without handing reaction events to Centaur', async () => {
    const message = await postUserMessage('Please react to this')
    const reaction = await slack.reactions.add({
      channel: CHANNEL_ID,
      timestamp: message.ts,
      name: 'eyes'
    })
    expect(reaction.ok).toBe(true)

    const response = await app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-emulate-reaction',
        event: {
          type: 'reaction_added',
          user: USER_ID,
          reaction: 'eyes',
          item: { type: 'message', channel: CHANNEL_ID, ts: message.ts }
        }
      })
    )

    expect(response.status).toBe(200)
    expect(centaur.workflowRuns).toHaveLength(0)
    const reactions = await slack.reactions.get({ channel: CHANNEL_ID, timestamp: message.ts })
    expect(reactions.message?.reactions?.[0]).toMatchObject({ name: 'eyes', count: 1 })
  })

  it('posts, updates, deletes, and reads Slack messages through Slackbot API routes', async () => {
    const post = await app.request('/api/slack/messages', {
      method: 'POST',
      headers: apiHeaders(),
      body: JSON.stringify({ channel: CHANNEL_ID, text: 'route-created' })
    })
    expect(post.status).toBe(200)
    const posted = (await post.json()) as { ts: string }

    const update = await app.request('/api/slack/messages', {
      method: 'PATCH',
      headers: apiHeaders(),
      body: JSON.stringify({ channel: CHANNEL_ID, ts: posted.ts, text: 'route-updated' })
    })
    expect(update.status).toBe(200)
    expect(await channelText()).toContain('route-updated')

    const replies = await app.request(
      `/api/slack/conversations/replies?channel=${CHANNEL_ID}&ts=${posted.ts}`,
      { headers: apiHeaders() }
    )
    expect(replies.status).toBe(200)
    expect(
      ((await replies.json()) as { messages: Array<{ text: string }> }).messages[0]?.text
    ).toBe('route-updated')

    const deleted = await app.request('/api/slack/messages', {
      method: 'DELETE',
      headers: apiHeaders(),
      body: JSON.stringify({ channel: CHANNEL_ID, ts: posted.ts })
    })
    expect(deleted.status).toBe(200)
    expect(await channelText()).not.toContain('route-updated')
  })

  it('renders agent sessions through patched Slack stream endpoints into Emulate history', async () => {
    const parent = await postUserMessage('Start session here')
    const opened = await app.request('/api/slack/agent-sessions', {
      method: 'POST',
      headers: apiHeaders(),
      body: JSON.stringify({
        channel: CHANNEL_ID,
        parent_ts: parent.ts,
        recipient_team_id: TEAM_ID,
        recipient_user_id: USER_ID,
        title: 'Centaur execution',
        header: 'base · codex'
      })
    })
    expect(opened.status).toBe(200)
    const { session_id: sessionId } = (await opened.json()) as { session_id: string }

    await app.request(`/api/slack/agent-sessions/${sessionId}/step`, {
      method: 'POST',
      headers: apiHeaders(),
      body: JSON.stringify({
        id: 'cmd-1',
        title: 'Command execution',
        status: 'in_progress',
        details: 'call demo ping'
      })
    })
    await app.request(`/api/slack/agent-sessions/${sessionId}/text`, {
      method: 'POST',
      headers: apiHeaders(),
      body: JSON.stringify({ markdown: 'Final answer from stream.' })
    })
    const done = await app.request(`/api/slack/agent-sessions/${sessionId}/done`, {
      method: 'POST',
      headers: apiHeaders(),
      body: JSON.stringify({})
    })
    expect(done.status).toBe(200)

    const text = await threadText(parent.ts)
    expect(text).toContain('Command execution')
    expect(text).toContain('Final answer from stream.')
  })

  it('polls final-delivery fallback and posts the completed result into Slack', async () => {
    const parent = await postUserMessage('Fallback target')
    centaur.deliveries.push({
      execution_id: 'exe-emulate-final',
      thread_key: `slack:${TEAM_ID}:${CHANNEL_ID}:${parent.ts}`,
      delivery: {
        platform: 'slack',
        channel: CHANNEL_ID,
        thread_ts: parent.ts,
        recipient_user_id: USER_ID,
        team_id: TEAM_ID
      },
      final_payload: {
        session_title: 'Recovered execution',
        result_text: 'Fallback result delivered.'
      }
    })

    await pollFinalDeliveriesOnce(
      {
        CENTAUR_API_URL: centaur.url,
        SLACKBOT_API_KEY: API_KEY,
        CENTAUR_API_KEY: undefined
      } as any,
      botSlack
    )

    expect(centaur.delivered).toContain('exe-emulate-final')
    expect(await threadText(parent.ts)).toContain('Fallback result delivered.')
  })
})

async function postUserMessage(text: string, threadTs?: string): Promise<{ ts: string }> {
  const response = await slack.chat.postMessage({
    channel: CHANNEL_ID,
    thread_ts: threadTs,
    text
  })
  expect(response.ok).toBe(true)
  return { ts: String(response.ts) }
}

async function postBotMessage(text: string, threadTs?: string): Promise<{ ts: string }> {
  const response = await botSlack.chat.postMessage({
    channel: CHANNEL_ID,
    thread_ts: threadTs,
    text
  })
  expect(response.ok).toBe(true)
  return { ts: String(response.ts) }
}

async function channelText(): Promise<string> {
  const history = await slack.conversations.history({ channel: CHANNEL_ID, limit: 100 })
  return (history.messages ?? []).map(message => message.text ?? '').join('\n')
}

async function threadText(threadTs: string): Promise<string> {
  const replies = await slack.conversations.replies({
    channel: CHANNEL_ID,
    ts: threadTs,
    limit: 100
  })
  return (replies.messages ?? []).map(message => message.text ?? '').join('\n')
}

function onlyRun(): WorkflowRunRequest {
  expect(centaur.workflowRuns).toHaveLength(1)
  return centaur.workflowRuns[0] as WorkflowRunRequest
}

function signedSlackEvent(input: {
  event_id: string
  event: Record<string, unknown>
}): RequestInit {
  const body = JSON.stringify({
    type: 'event_callback',
    team_id: TEAM_ID,
    event_id: input.event_id,
    event: input.event
  })
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const signature = `v0=${createHmac('sha256', SIGNING_SECRET)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')}`
  return {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-slack-request-timestamp': timestamp,
      'x-slack-signature': signature
    },
    body
  }
}

function apiHeaders(): Record<string, string> {
  return {
    authorization: `Bearer ${API_KEY}`,
    'content-type': 'application/json'
  }
}

function waitUntilContext(waits: Promise<unknown>[]): any {
  return {
    waitUntil: (promise: Promise<unknown>) => waits.push(promise),
    passThroughOnException: () => {},
    props: {}
  }
}

async function createFakeCentaur() {
  const workflowRuns: WorkflowRunRequest[] = []
  const deliveries: FakeDelivery[] = []
  const delivered: string[] = []
  const failed: string[] = []
  const port = await preferredPort(4014)
  const server = Bun.serve({
    port,
    async fetch(request: Request) {
      const url = new URL(request.url)
      if (url.pathname === '/workflows/runs') {
        workflowRuns.push((await request.json()) as WorkflowRunRequest)
        return Response.json({ ok: true, run_id: `wfr-${workflowRuns.length}` })
      }
      if (url.pathname === '/agent/final-deliveries/claim') {
        return Response.json({ deliveries: deliveries.splice(0) })
      }
      const deliveredMatch = /^\/agent\/final-deliveries\/([^/]+)\/delivered$/.exec(url.pathname)
      if (deliveredMatch) {
        delivered.push(decodeURIComponent(deliveredMatch[1] ?? ''))
        return Response.json({ ok: true })
      }
      const failedMatch = /^\/agent\/final-deliveries\/([^/]+)\/failed$/.exec(url.pathname)
      if (failedMatch) {
        failed.push(decodeURIComponent(failedMatch[1] ?? ''))
        return Response.json({ ok: true })
      }
      return Response.json({ ok: false, error: 'not_found' }, { status: 404 })
    }
  })
  return {
    url: `http://localhost:${server.port}`,
    workflowRuns,
    deliveries,
    delivered,
    failed,
    reset() {
      workflowRuns.length = 0
      deliveries.length = 0
      delivered.length = 0
      failed.length = 0
    },
    async close() {
      await server.stop()
    }
  }
}

async function preferredPort(port: number): Promise<number> {
  if (await isPortOpen(port)) {
    for (let candidate = port + 1; candidate < port + 100; candidate++) {
      if (!(await isPortOpen(candidate))) return candidate
    }
    throw new Error(`No available port near ${port}`)
  }
  return port
}

async function isPortOpen(port: number): Promise<boolean> {
  return new Promise(resolve => {
    const socket = connect(port, '127.0.0.1')
    socket.once('connect', () => {
      socket.destroy()
      resolve(true)
    })
    socket.once('error', () => resolve(false))
    socket.setTimeout(250, () => {
      socket.destroy()
      resolve(false)
    })
  })
}
