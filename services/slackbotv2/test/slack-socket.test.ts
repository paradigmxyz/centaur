import { describe, expect, it } from 'bun:test'
import type { SlackSocketModeHandlers } from '@chat-adapter/slack'
import { createMemoryState } from '@chat-adapter/state-memory'
import type { Logger } from 'chat'
import { createSlackbotV2, type SlackbotV2 } from '../src/index'
import { resetSlackbotMetricsForTests, slackbotMetrics } from '../src/metrics'
import { createSlackSocketRunner } from '../src/slack-socket'
import type { SlackbotV2Options } from '../src/types'

const BOT_USER_ID = 'U000000001'
const HOME_TEAM_ID = 'T000000001'
const EXTERNAL_TEAM_ID = 'T999999999'
const CHANNEL_ID = 'C000000001'

const silentLogger: Logger = {
  debug: () => {},
  info: () => {},
  warn: () => {},
  error: () => {},
  child: () => silentLogger
}

/** Stands in for the adapter's WebSocket: hands the runner's handlers back. */
function fakeTransport(connect?: () => Promise<unknown>) {
  let handlers: SlackSocketModeHandlers | undefined
  return {
    get handlers(): SlackSocketModeHandlers {
      if (!handlers) throw new Error('socket transport never connected')
      return handlers
    },
    transport: {
      connect: async (next: SlackSocketModeHandlers) => {
        handlers = next
        if (connect) return await connect()
        next.onStateChange?.('connected')
        return undefined
      },
      disconnect: async () => undefined
    }
  }
}

/** One `events_api` envelope, shaped the way Slack sends it over the socket. */
function eventsApiEnvelope(event: Record<string, unknown>): Record<string, unknown> {
  return {
    event,
    event_id: `Ev${Math.random().toString(36).slice(2, 10)}`,
    event_time: 1_700_000_000,
    team_id: HOME_TEAM_ID,
    type: 'event_callback'
  }
}

function externalMention(): Record<string, unknown> {
  return eventsApiEnvelope({
    channel: CHANNEL_ID,
    text: `<@${BOT_USER_ID}> ship it`,
    ts: '1700000000.000100',
    type: 'app_mention',
    user: 'UEXTERNAL',
    user_team: EXTERNAL_TEAM_ID
  })
}

function externalBlockAction(): Record<string, unknown> {
  return {
    actions: [{ action_id: 'centaur_stop', action_ts: '1700000000.000200' }],
    channel: { id: CHANNEL_ID },
    team: { id: HOME_TEAM_ID },
    type: 'block_actions',
    user: { id: 'UEXTERNAL', team_id: EXTERNAL_TEAM_ID }
  }
}

function socketBot(overrides: Partial<SlackbotV2Options> = {}): {
  bot: SlackbotV2
  fake: ReturnType<typeof fakeTransport>
  requests: string[]
} {
  const fake = fakeTransport()
  const requests: string[] = []
  const bot = createSlackbotV2({
    apiUrl: 'http://session.test/',
    appToken: 'xapp-1-socket-test',
    botToken: 'xoxb-socket-test',
    botUserId: BOT_USER_ID,
    fetch: (async (input: RequestInfo | URL) => {
      requests.push(String(input))
      return Response.json({ ok: true })
    }) as SlackbotV2Options['fetch'],
    logger: silentLogger,
    recoverRenderObligationsOnStart: false,
    slackApiUrl: 'http://slack.test/api/',
    slackHomeTeamId: HOME_TEAM_ID,
    socketMode: true,
    socketTransport: fake.transport,
    state: createMemoryState(),
    ...overrides
  })
  return { bot, fake, requests }
}

async function deliver(
  bot: SlackbotV2,
  fake: ReturnType<typeof fakeTransport>,
  eventType: string,
  body: Record<string, unknown>
): Promise<void> {
  await bot.socket!.start()
  // Slack is acked before the handoff finishes, so the dispatch is not awaited
  // here either; callers wait for the effect they expect.
  void fake.handlers.onEvent({ ack: async () => {}, body, eventType })
  await Bun.sleep(50)
}

async function waitFor(condition: () => boolean, timeoutMs = 2_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (condition()) return true
    await Bun.sleep(10)
  }
  return condition()
}

function ignoredRequests(route: string, eventType: string): number {
  const pattern = new RegExp(
    `^slackbotv2_slack_webhook_requests_total\\{route="${route}",event_type="${eventType}",`
      + 'outcome="ignored"\\} (\\d+)$',
    'm'
  )
  const match = pattern.exec(slackbotMetrics.expose())
  return match?.[1] ? Number.parseInt(match[1], 10) : 0
}

describe('Slackbot socket mode', () => {
  it('rejects Slack routes reached from outside the process', async () => {
    const { bot } = socketBot()

    const response = await bot.app.fetch(
      new Request('http://slackbotv2.test/api/slack/events', {
        body: JSON.stringify(
          eventsApiEnvelope({
            channel: CHANNEL_ID,
            text: 'hello',
            type: 'app_mention',
            user: 'UHOME'
          })
        ),
        headers: { 'content-type': 'application/json' },
        method: 'POST'
      })
    )

    // No signing secret is configured in socket mode, so the routes must not
    // become an unauthenticated way in.
    expect(response.status).toBe(401)
  })

  it('runs socket events through the external-org allowlist', async () => {
    resetSlackbotMetricsForTests()
    const blocked = socketBot({ allowedExternalTeamIds: [] })
    await deliver(blocked.bot, blocked.fake, 'events_api', externalMention())

    expect(ignoredRequests('/api/slack/events', 'app_mention')).toBe(1)
    expect(blocked.requests).toEqual([])
  })

  it('runs socket block actions through the external-org allowlist', async () => {
    resetSlackbotMetricsForTests()
    const blocked = socketBot({ allowedExternalTeamIds: [] })
    await deliver(blocked.bot, blocked.fake, 'interactive', externalBlockAction())

    expect(ignoredRequests('/api/slack/actions', 'block_actions')).toBe(1)
    expect(blocked.requests).toEqual([])

    const allowed = socketBot({ allowedExternalTeamIds: [EXTERNAL_TEAM_ID] })
    await deliver(allowed.bot, allowed.fake, 'interactive', externalBlockAction())

    expect(await waitFor(() => allowed.requests.some(url => url.includes('/api/workflows/events'))))
      .toBe(true)
  })

  it('auto-joins channels created while running on the socket', async () => {
    const { bot, fake, requests } = socketBot({ autoJoinCreatedChannels: true })

    await deliver(
      bot,
      fake,
      'events_api',
      eventsApiEnvelope({ channel: { id: 'C0NEWCHANNEL', name: 'new' }, type: 'channel_created' })
    )

    expect(await waitFor(() => requests.some(url => url.includes('conversations.join')))).toBe(true)
  })

})

describe('Slack socket runner', () => {
  it('replays an events_api envelope verbatim onto the events route', async () => {
    const fake = fakeTransport()
    const replayed: Request[] = []
    const socket = createSlackSocketRunner({
      ...fake.transport,
      dispatch: async request => {
        replayed.push(request)
        return new Response('ok')
      },
      logger: silentLogger,
      loopbackToken: 'token-abc'
    })
    await socket.start()

    const body = externalMention()
    let acked = false
    await fake.handlers.onEvent({
      ack: async () => {
        // A handoff can outlast Slack's 3s ack budget; acking first keeps the
        // connection alive.
        acked = replayed.length === 0
      },
      body,
      eventType: 'events_api'
    })

    expect(acked).toBe(true)
    expect(replayed).toHaveLength(1)
    expect(new URL(replayed[0]!.url).pathname).toBe('/api/slack/events')
    // Verbatim matters: the gate reads team_id off the envelope and the adapter
    // reads authorizations and is_ext_shared_channel.
    expect(await replayed[0]!.json()).toEqual(body)
  })

  it('drops a redelivered envelope instead of dispatching it twice', async () => {
    const fake = fakeTransport()
    let dispatches = 0
    const socket = createSlackSocketRunner({
      ...fake.transport,
      dispatch: async () => {
        dispatches += 1
        return new Response('ok')
      },
      logger: silentLogger,
      loopbackToken: 'token-abc'
    })
    await socket.start()

    await fake.handlers.onEvent({
      ack: async () => {},
      body: externalMention(),
      eventType: 'events_api',
      retryNum: 1
    })

    expect(dispatches).toBe(0)
  })

})
