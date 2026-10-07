import { createHmac } from 'node:crypto'
import {
  createServer,
  request as httpRequest,
  type IncomingMessage,
  type Server as HttpServer,
  type ServerResponse
} from 'node:http'
import { connect } from 'node:net'
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'bun:test'
import { WebClient } from '@slack/web-api'
import { createEmulator, type Emulator } from 'emulate'
import { createMemoryState } from '@chat-adapter/state-memory'
import type { ServerNotification } from '@centaur/harness-events'
import {
  createSlackbotV2,
  normalizeSlackText,
  type SlackbotV2,
  type SlackbotV2AppendMessagesRequest,
  type SlackbotV2ApiMessage,
  type SlackbotV2BlockActionPayload,
  type SlackbotV2CreateSessionRequest,
  type SlackbotV2ExecuteSessionRequest,
  type SlackbotV2SessionMessage
} from '../src/index'
import { clearRequesterIdentityCacheForTests } from '../src/session-api'
import { slackbotMetrics } from '../src/metrics'
import { createOpenAiMessageOverridesStrategy } from '../src/message-overrides-strategy'
import { modelDisplayName } from '../src/response-context'
import claudeSettings from '../../../harness/claude/settings.json'

const BOT_TOKEN = 'xoxb-slackbotv2-emulate'
const USER_TOKEN = 'xoxp-slackbotv2-user'
const USER_B_TOKEN = 'xoxp-slackbotv2-user-b'
const SIGNING_SECRET = 'slackbotv2-signing-secret'
const BOT_USER_ID = 'U000000001'
const USER_ID = 'USLACKBOTV2USER'
const USER_B_ID = 'USLACKBOTV2USERB'
const TEAM_ID = 'T000000001'
const CHANNEL_ID = 'C000000001'
/** How real Slack renders a streamed message whose stream broke or was never stopped. */
const BROKEN_STREAM_TEXT = ':warning: Something went wrong'
const SLACK_MARKDOWN_TEXT_MAX_CHARS = 12_000

function contentTextWithHeading(
  content: Array<{ text?: string; type: string }>,
  heading: string
): string {
  return content.find(part => typeof part.text === 'string' && part.text.includes(heading))?.text
    ?? ''
}

describe('normalizeSlackText', () => {
  it('preserves Slack channel IDs when rendering labeled channel mentions', () => {
    expect(normalizeSlackText('<#C0AJ07U8Z1N|eng-centaur>')).toBe(
      '#eng-centaur (C0AJ07U8Z1N)'
    )
  })
})

let emulator: Emulator
let slackApi: PatchedSlackApi
let codexApi: MockSessionApi
let slack: WebClient
let slackB: WebClient
let slackBot: WebClient
let slackApiUrl: string
let bot: SlackbotV2

beforeAll(async () => {
  emulator = await createEmulator({
    service: 'slack',
    port: await availablePort(4043),
    seed: {
      tokens: {
        [BOT_TOKEN]: {
          login: BOT_USER_ID,
          scopes: [
            'assistant:write',
            'channels:join',
            'channels:read',
            'chat:write',
            'im:read',
            'im:write',
            'im:history',
            'users:read'
          ]
        },
        [USER_TOKEN]: {
          login: USER_ID,
          scopes: ['chat:write', 'channels:read', 'users:read']
        },
        [USER_B_TOKEN]: {
          login: USER_B_ID,
          scopes: ['chat:write', 'channels:read', 'users:read']
        }
      },
      slack: {
        team: { name: 'Slackbot V2', domain: 'slackbot-v2' },
        users: [
          { name: 'tester', real_name: 'Test User', email: 'tester@example.com' },
          { name: 'builder', real_name: 'Build User', email: 'builder@example.com' }
        ],
        channels: [{ name: 'slackbot-v2' }],
        bots: [{ name: 'centaur' }],
        signing_secret: SIGNING_SECRET
      }
    }
  })
  slackApi = await startPatchedSlackApi(emulator.url)
  codexApi = await startMockCodexApi()
  slackApiUrl = `${slackApi.url}/api/`
  slack = new WebClient(USER_TOKEN, { slackApiUrl })
  slackB = new WebClient(USER_B_TOKEN, { slackApiUrl })
  slackBot = new WebClient(BOT_TOKEN, { slackApiUrl })
})

beforeEach(() => {
  clearRequesterIdentityCacheForTests()
  emulator.reset()
  slackApi.reset()
  codexApi.reset()
  bot = createTestBot()
})

afterAll(async () => {
  await codexApi?.close()
  await slackApi?.close()
  await emulator?.close()
})

describe('slackbotv2', () => {
  // Legacy (non-agent-view) DMs run end to end in e2e/tests/context.test.ts.
  it('routes agent-view DM roots and replies to their own sessions', async () => {
    bot = createTestBot({ agentViewEnabled: true })
    const members = await slackBot.users.list({})
    const userId = members.members?.find(member => member.name === 'tester')?.id
    expect(userId).toBeDefined()
    const dm = await slackBot.conversations.open({ users: userId! })
    expect(dm.channel?.id).toBeDefined()
    const channel = dm.channel!.id!
    // A prior legacy DM leaves this subscription behind when the toggle is
    // enabled. Agent mode must still use the new user message as its root.
    await bot.chat.getState().subscribe(`slack:${channel}:`)
    const roots: string[] = []
    for (let turn = 0; turn < 2; turn++) {
      const posted = await slackBot.chat.postMessage({ channel, text: `DM request ${turn}` })
      expect(posted.ts).toBeDefined()
      roots.push(posted.ts!)
      const delivery = await deliverEvent({
        type: 'message', channel_type: 'im', channel, user: USER_ID,
        ts: posted.ts, text: `DM request ${turn}`
      })
      expect(delivery.response.status).toBe(200)
      await delivery.finished()
      const delivered = await slackBot.conversations.replies({ channel, ts: posted.ts! })
      const answerText = (delivered.messages ?? [])
        .map(message => [message.text ?? '', blocksText(message.blocks)].join('\n'))
        .join('\n')
      expect(answerText).toContain(`Executed request ${turn + 1}.`)
    }
    expect(codexApi.executes.map(request => request.threadKey)).toEqual(
      roots.map(ts => `slack:${channel}:${ts}`)
    )
    const reply = await slackBot.chat.postMessage({ channel, thread_ts: roots[0], text: 'Follow up' })
    const delivery = await deliverEvent({
      type: 'message', channel_type: 'im', channel, user: USER_ID,
      ts: reply.ts, thread_ts: roots[0], text: 'Follow up'
    })
    expect(delivery.response.status).toBe(200)
    await delivery.finished()
    expect(codexApi.executes.at(-1)?.threadKey).toBe(`slack:${channel}:${roots[0]}`)
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(true)
    expect(slackApi.calls.some(call => call.method === 'agents.sessions.rename')).toBe(true)
  })

  it('accepts Slack events on the legacy route', async () => {
    const parent = await postUserMessage('Legacy route context.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> use the legacy route`, parent.ts)
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      thread_ts: parent.ts,
      text: `<@${BOT_USER_ID}> use the legacy route`
    }, { route: '/api/slack/events' })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.executes[0]?.threadKey).toBe(threadKey(parent.ts))
  })

  it('joins newly-created public channels', async () => {
    bot = createTestBot({ autoJoinCreatedChannels: true })
    const delivery = await deliverEvent({
      type: 'channel_created',
      channel: {
        id: 'CNEWCHANNEL',
        name: 'new-channel',
        created: 1700000000,
        creator: USER_ID
      },
      team: TEAM_ID
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()
    expect(slackApi.calls).toContainEqual({
      method: 'conversations.join',
      body: { channel: 'CNEWCHANNEL' }
    })
    expect(codexApi.creates).toHaveLength(0)
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)
  })

  it('does not join newly-created public channels when auto-join is disabled', async () => {
    const delivery = await deliverEvent({
      type: 'channel_created',
      channel: {
        id: 'CDISABLEDCHANNEL',
        name: 'disabled-channel',
        created: 1700000000,
        creator: USER_ID
      },
      team: TEAM_ID
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()
    expect(slackApi.calls.filter(call => call.method === 'conversations.join')).toEqual([])
  })

  it('logs conversations.join failures without failing the webhook', async () => {
    const logs: CapturedLog[] = []
    bot = createTestBot({ autoJoinCreatedChannels: true, logger: captureLogger(logs) })
    slackApi.respondToNextConversationsJoin(500, { ok: false, error: 'server_error' })
    const delivery = await deliverEvent({
      type: 'channel_created',
      channel: {
        id: 'CFAILEDCHANNEL',
        name: 'failed-channel',
        created: 1700000000,
        creator: USER_ID
      },
      team: TEAM_ID
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()
    expect(slackApi.calls).toContainEqual({
      method: 'conversations.join',
      body: { channel: 'CFAILEDCHANNEL' }
    })
    expect(hasLog(logs, 'slackbotv2_channel_created_join_failed')).toBe(true)
  })

  it('logs already joined from Slack warning responses', async () => {
    for (const [suffix, slackResponse] of [
      ['top-level', { ok: true, warning: 'already_in_channel' }],
      [
        'metadata',
        { ok: true, response_metadata: { warnings: ['already_in_channel'] } }
      ]
    ] as const) {
      const logs: CapturedLog[] = []
      bot = createTestBot({
        allowedExternalTeamIds: ['TEVENTTEAM'],
        autoJoinCreatedChannels: true,
        logger: captureLogger(logs)
      })
      slackApi.respondToNextConversationsJoin(200, slackResponse)
      const delivery = await deliverEvent({
        type: 'channel_created',
        channel: {
          id: `CALREADYJOINED${suffix.toUpperCase().replace('-', '')}`,
          name: `already-joined-${suffix}`,
          created: 1700000000,
          creator: USER_ID
        },
        team: 'TEVENTTEAM'
      })

      expect(delivery.response.status).toBe(200)
      await delivery.finished()
      expect(
        logData(logs, 'slackbotv2_channel_created_join_complete')?.already_joined
      ).toBe(true)
      expect(
        logData(logs, 'slackbotv2_channel_created_join_complete')?.team_id
      ).toBe('TEVENTTEAM')
      expect(hasLog(logs, 'slackbotv2_channel_created_join_failed')).toBe(false)
    }
  })

  it('durably hands off workflow buttons before acknowledging and retries failed acceptance', async () => {
    const requests: Record<string, unknown>[] = []
    const feedback: unknown[] = []
    let release: (() => void) | undefined
    const held = new Promise<void>(resolve => { release = resolve })
    let fail = true
    let created = true
    bot = createTestBot({
      fetch: async (input, init) => {
        if (String(input).endsWith('/api/workflows/actions/invoke')) {
          requests.push(JSON.parse(String(init?.body)))
          await held
          return fail
            ? Response.json({ error: 'temporarily unavailable' }, { status: 503 })
            : Response.json({ ok: true, run_id: 'run-click-1', task_id: 'task-click-1', created, status: 'queued' })
        }
        if (String(input).endsWith('/chat.postEphemeral')) {
          feedback.push(Object.fromEntries(new URLSearchParams(String(init?.body))))
        }
        return globalThis.fetch(input, init)
      }
    })
    const payload = {
      type: 'block_actions', team: { id: TEAM_ID },
      user: { id: USER_ID, username: 'tester', team_id: TEAM_ID },
      channel: { id: CHANNEL_ID }, message: {
        ts: '1700000003.000200', text: 'Approve this release?',
        blocks: [
          { type: 'section', text: { type: 'mrkdwn', text: 'Approve this release?' } },
          { type: 'actions', elements: [{ type: 'button', text: { type: 'plain_text', text: 'Approve' },
            action_id: 'centaur.workflow.action:00000000-0000-0000-0000-000000000001:approve',
            value: 'v1.opaque-signed-payload.signature' }] }
        ]
      },
      actions: [{
        action_id: 'centaur.workflow.action:00000000-0000-0000-0000-000000000001:approve',
        action_ts: '1700000004.000200', type: 'button',
        value: 'v1.opaque-signed-payload.signature'
      }]
    }
    const waits: Promise<unknown>[] = []
    let acknowledged = false
    const first = Promise.resolve(bot.app.request('/api/slack/actions', signedSlackInteraction(payload), {}, waitUntilContext(waits)))
      .then(response => { acknowledged = true; return response })
    await waitFor(() => requests.length === 1)
    expect(acknowledged).toBe(false)
    release?.()
    expect((await first).status).toBe(503)
    fail = false
    const retry = await bot.app.request('/api/slack/actions', signedSlackInteraction(payload), {}, waitUntilContext(waits))
    expect(retry.status).toBe(200)
    expect(await retry.text()).toBe('')
    created = false
    const duplicate = await bot.app.request('/api/slack/actions', signedSlackInteraction(payload), {}, waitUntilContext(waits))
    expect(duplicate.status).toBe(200)
    expect(await duplicate.text()).toBe('')
    await Promise.all(waits)
    expect(feedback).toEqual([])
    expect(requests).toHaveLength(3)
    expect(requests[0]).toEqual({
      button: payload.actions[0]?.value,
      message: { text: payload.message.text, blocks: payload.message.blocks },
      idempotency_key: expect.stringMatching(/^slack\.button:[0-9a-f]{64}$/),
      click: {
          id: '00000000-0000-0000-0000-000000000001', action: 'approve',
          action_ts: payload.actions[0]?.action_ts, channel_id: CHANNEL_ID,
          message_ts: payload.message.ts, team_id: TEAM_ID, user_id: USER_ID
      }
    })
    expect(requests[1]).toEqual(requests[0])
    expect(requests[2]).toEqual(requests[0])
    expect(codexApi.workflowEvents).toHaveLength(0)
  })

  it('applies the external-org allowlist to Slack block actions', async () => {
    const interaction = signedSlackInteraction({
      type: 'block_actions',
      team: { id: TEAM_ID },
      user: { id: USER_ID, username: 'tester', team_id: 'TEXTERNAL' },
      channel: { id: CHANNEL_ID },
      message: { ts: '1700000003.000200', thread_ts: '1700000003.000100' },
      actions: [{ action_id: 'deploy.approve', type: 'button', value: 'release-42' }]
    })

    const denied = await bot.app.request('/api/webhooks/slack', interaction)
    expect(denied.status).toBe(200)
    expect(codexApi.workflowEvents).toHaveLength(0)

    bot = createTestBot({ allowedExternalTeamIds: ['TEXTERNAL'] })
    const waits: Promise<unknown>[] = []
    const allowed = await bot.app.request(
      '/api/webhooks/slack',
      interaction,
      {},
      waitUntilContext(waits)
    )
    expect(allowed.status).toBe(200)
    await Promise.all(waits)
    expect(codexApi.workflowEvents).toHaveLength(1)
  })

  it('deduplicates Slack block action retries by action timestamp', async () => {
    const interaction = signedSlackInteraction({
      type: 'block_actions',
      team: { id: TEAM_ID },
      user: { id: USER_ID, username: 'tester', team_id: TEAM_ID },
      channel: { id: CHANNEL_ID },
      message: { ts: '1700000004.000200', thread_ts: '1700000004.000100' },
      actions: [
        {
          action_id: 'deploy.approve',
          action_ts: '1700000005.000300',
          type: 'button',
          value: 'release-42'
        }
      ]
    })

    for (let attempt = 0; attempt < 2; attempt += 1) {
      const waits: Promise<unknown>[] = []
      const response = await bot.app.request(
        '/api/webhooks/slack',
        interaction,
        {},
        waitUntilContext(waits)
      )
      expect(response.status).toBe(200)
      await Promise.all(waits)
    }

    expect(codexApi.workflowEvents).toHaveLength(1)
  })

  // Collecting unmentioned replies into the next turn runs end to end in
  // e2e/tests/context.test.ts; this covers how Slack renders the turn.
  it('renders a mention with an attachment as a plan stream with status and title', async () => {
    const parent = await postUserMessage('The deploy context is above.')
    const firstMention = await postUserMessage(
      `<@${BOT_USER_ID}> run with this screenshot`,
      parent.ts
    )
    const fileUrl = 'https://files.slack.com/files/captured.png'
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: firstMention.ts,
      thread_ts: parent.ts,
      text: `<@${BOT_USER_ID}> run with this screenshot`,
      files: [
        {
          id: 'F-captured',
          mimetype: 'image/png',
          name: 'captured.png',
          original_h: 600,
          original_w: 800,
          size: 16,
          url_private: fileUrl
        }
      ]
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()

    const firstAttachment = codexApi.appends[0]!.body.messages
      .flatMap(message => message.parts)
      .find(part => isRecord(part) && part.type === 'attachment')
    expect(firstAttachment).toEqual(
      expect.objectContaining({
        attachment_type: 'image',
        dataBase64: Buffer.from('captured-image').toString('base64'),
        mimeType: 'image/png',
        name: 'captured.png',
        type: 'attachment',
        url: fileUrl
      })
    )
    const firstInputLine = JSON.parse(codexApi.executes[0]!.body.input_lines[0]!) as Record<string, unknown>
    expect(firstInputLine).toEqual(
      expect.objectContaining({
        message: expect.objectContaining({
          content: expect.arrayContaining([
            expect.objectContaining({
              attachment_type: 'image',
              dataBase64: Buffer.from('captured-image').toString('base64'),
              mimeType: 'image/png',
              name: 'captured.png',
              type: 'attachment'
            })
          ])
        })
      })
    )
    expect(JSON.stringify(firstInputLine)).not.toContain('data:image/png;base64')

    expectSlackPlanStreamShape(slackApi.calls, {
      answers: ['Executed request 1.'],
      parentTs: parent.ts
    })
    const assistantStatuses = slackApi.calls
      .filter(call => call.method === 'assistant.threads.setStatus')
      .map(call => stringField(call.body.status))
    expect(assistantStatuses[0]).toBe('Thinking...')
    expect(assistantStatuses.at(-1)).toBe('')
    expect(
      slackApi.calls
        .filter(call => call.method === 'assistant.threads.setTitle')
        .map(call => stringField(call.body.title))
    ).toEqual(['run with this screenshot', 'Codex request 1'])

    const text = await threadText(parent.ts)
    expect(text).toContain('Implementation plan')
    expect(text).toContain('Inspect App Server events')
    expect(text).not.toContain('Checking the command output')
    expect(text).not.toContain('Inspecting the event stream')
    expect(text).not.toContain('Thinking')
    expect(text).toContain('Command execution')
    expect(text).toContain('pnpm test')
    expect(text).not.toContain('tests passed')
    const renderedReplies = (await threadTexts(parent.ts)).filter(reply =>
      reply.includes('Executed request')
    )
    expect(renderedReplies).toHaveLength(1)
    expectSlackRenderedReply(renderedReplies[0]!, 'Executed request 1.')
  })

  // The paragraph break (`\n\n`) after the model value is deliberate: the
  // unpatched chat SDK dropped it, gluing the value to the next word
  // (`fablefirst`); this exercises the patched extractPlainText end to end.
  for (const scenario of ['classifier', 'harness flag', 'model flag', 'channel default', 'sticky thread', 'rollout', 'cleared model']) {
    it(`does not route to a disabled harness via ${scenario}`, async () => {
      const state = createMemoryState()
      await state.connect()
      bot = createTestBot({
        state,
        enabledHarnesses: ['codex', 'claudecode'],
        defaultHarnessType: scenario === 'cleared model' ? 'claudecode' : undefined,
        channelDefaults: scenario === 'channel default' || scenario === 'cleared model'
          ? { [CHANNEL_ID]: { harnessType: 'amp', model: 'fast' } } : undefined,
        codexNanocodexRolloutPercent: scenario === 'rollout' ? 100 : 0,
        messageOverridesStrategy: scenario === 'classifier'
          ? async () => ({ overrides: { harnessType: 'amp', model: 'fast' } }) : undefined
      })
      const parent = await postUserMessage('Harness policy thread context.')
      if (scenario === 'sticky thread') {
        await state.set(`thread-state:${threadKey(parent.ts)}`, {
          harnessType: 'amp', model: 'fast', provider: 'responses'
        })
      }
      const text = scenario === 'harness flag' ? '--amp --model fast review this'
        : scenario === 'model flag' ? '--model fast review this'
        : scenario === 'cleared model' ? '--codex review this'
        : 'get average scanner fast runtime and scanner superfast runtime'
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs: parent.ts })
      expect(mention.response.status).toBe(200)
      await mention.finished()
      expect(codexApi.creates.map(create => create.body.harness_type)).toEqual(['codex'])
      expect(codexApi.executes).toHaveLength(1)
      const execute = codexApi.executes[0]!.body
      expect(JSON.parse(execute.input_lines.at(-1)!).model).toBeUndefined()
      expect(JSON.parse(execute.input_lines.at(-1)!).provider).toBeUndefined()
      expect(execute.metadata.harness_type).toBe('codex')
      if (scenario !== 'rollout') {
        expect(await state.get(`thread-state:${threadKey(parent.ts)}`)).toEqual(
          expect.objectContaining({ harnessType: 'codex' })
        )
      }
      const replies = await slack.conversations.replies({ channel: CHANNEL_ID, ts: parent.ts })
      expect(JSON.stringify(replies.messages?.filter(message => message.user === BOT_USER_ID)))
        .not.toContain('Amp')
    })
  }
  it('pins sticky persona state without labeling a pinned mismatch as unavailable', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.queueCreateResponse({
      harness_switched: false,
      harness_type: 'claudecode',
      persona_id: 'old'
    })

    const parent = await postUserMessage('Thread default context.')
    const runMention = async (text: string) => {
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs: parent.ts })
      expect(mention.response.status).toBe(200)
      await mention.finished()
    }

    await runMention('--claude --persona=eng first pass')
    await runMention('--persona=eng continue with a later selector')

    expect(codexApi.creates.map(create => create.body.persona_id)).toEqual(['eng', 'old'])
    expect(stopStreamBlocksText(slackApi.calls)).not.toContain("isn't available")
    const state = await sharedState.get<Record<string, unknown>>(
      `thread-state:${threadKey(parent.ts)}`
    )
    expect(state).toEqual(expect.objectContaining({ personaId: 'old' }))
  })

  it('applies a per-channel default persona below an explicit persona flag', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({
      state: sharedState,
      channelDefaults: { [CHANNEL_ID]: { personaId: 'invest' } }
    })

    const runThread = async (text: string) => {
      const parent = await postUserMessage(`Context for ${text}.`)
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs: parent.ts })
      expect(mention.response.status).toBe(200)
      await mention.finished()
    }

    await runThread('review this deal')
    await runThread('--persona=eng fix this bug')

    expect(codexApi.creates.map(create => create.body.persona_id)).toEqual(['invest', 'eng'])
  })

  it('pins a channel default persona to the thread even after the channel default changes', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    const logs: CapturedLog[] = []
    const channelDefaults: Record<string, { personaId: string }> = {
      [CHANNEL_ID]: { personaId: 'invest' }
    }
    bot = createTestBot({ state: sharedState, channelDefaults, logger: captureLogger(logs) })

    const parent = await postUserMessage('Channel persona pin context.')
    const runMention = async (text: string) => {
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs: parent.ts })
      expect(mention.response.status).toBe(200)
      await mention.finished()
    }

    await runMention('first pass')
    channelDefaults[CHANNEL_ID] = { personaId: 'eng' }
    await runMention('second pass')

    expect(codexApi.creates.map(create => create.body.persona_id)).toEqual(['invest', 'invest'])
    const state = await sharedState.get<Record<string, unknown>>(
      `thread-state:${threadKey(parent.ts)}`
    )
    expect(state).toEqual(expect.objectContaining({ personaId: 'invest' }))
    expect(
      logs
        .filter(log => log.event === 'slackbotv2_forward_persona_resolved')
        .map(log => (log.data as Record<string, unknown>).persona_source)
    ).toEqual(['channel', 'thread'])
  })

  it('does not apply a channel default persona to a thread pinned without one', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({
      state: sharedState,
      channelDefaults: { [CHANNEL_ID]: { personaId: 'invest' } }
    })

    const parent = await postUserMessage('No-persona thread context.')
    await sharedState.set(`thread-state:${threadKey(parent.ts)}`, { personaId: null })
    const mention = await sendMessage(`<@${BOT_USER_ID}> keep going`, { threadTs: parent.ts })
    expect(mention.response.status).toBe(200)
    await mention.finished()

    expect(codexApi.creates).toHaveLength(1)
    expect(codexApi.creates[0]!.body.persona_id).toBeUndefined()
  })

  it('reports a fallback for a stale sticky persona on a plain message', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.queueCreateResponse({
      harness_switched: false,
      harness_type: 'codex',
      persona_id: 'eng',
      unavailable_requested_persona_id: 'honk'
    })

    const parent = await postUserMessage('Thread default context.')
    await sharedState.set(`thread-state:${threadKey(parent.ts)}`, { personaId: 'honk' })
    const mention = await sendMessage(`<@${BOT_USER_ID}> start with the fallback`, {
      threadTs: parent.ts
    })
    expect(mention.response.status).toBe(200)
    await mention.finished()

    expect(codexApi.creates[0]?.body.persona_id).toBe('honk')
    expect(stopStreamBlocksText(slackApi.calls)).toContain(
      `Persona "honk" isn't available. Using "eng" instead.`
    )
  })

  it('clears a sticky model rejected by the harness and accepts a later override', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false

    const parent = await postUserMessage('Thread default context.')
    const runTurn = async (text: string, terminalEvent: string, data: unknown) => {
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs: parent.ts })
      expect(mention.response.status).toBe(200)
      await waitFor(() => codexApi.eventRequests.length === codexApi.executes.length, 3000)
      codexApi.emitSessionEvent(threadKey(parent.ts), terminalEvent, data)
      await mention.finished()
    }

    await runTurn('--model 5.6-sol first', 'session.execution_failed', {
      error: JSON.stringify({
        type: 'invalid_request_error',
        code: 'model_not_found',
        message: "The requested model '5.6-sol' does not exist.",
        param: 'model'
      })
    })
    expect(
      await sharedState.get<Record<string, unknown>>(`thread-state:${threadKey(parent.ts)}`)
    ).toEqual(expect.objectContaining({ model: null }))
    expect(await threadText(parent.ts)).toContain("The requested model '5.6-sol' does not exist.")

    await runTurn(
      'continue without flags',
      'session.execution_completed',
      { result_text: 'Recovered on the default model.' }
    )
    await runTurn(
      '--model gpt-5.4 use this model',
      'session.execution_completed',
      { result_text: 'Accepted the replacement model.' }
    )

    const models = codexApi.executes.map(execute => {
      const input = JSON.parse(execute.body.input_lines.at(-1) ?? '{}') as Record<string, unknown>
      return input.model
    })
    expect(models).toEqual(['5.6-sol', undefined, 'gpt-5.4'])
    expect(
      await sharedState.get<Record<string, unknown>>(`thread-state:${threadKey(parent.ts)}`)
    ).toEqual(expect.objectContaining({ model: 'gpt-5.4' }))
  })

  it('keeps a top-level harness flag pinned when the LLM strategy guesses another harness', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    let strategyRequestCount = 0
    bot = createTestBot({
      defaultHarnessType: 'claudecode',
      messageOverridesStrategy: createOpenAiMessageOverridesStrategy({
        apiKey: 'test-key',
        fetch: (async () => {
          strategyRequestCount += 1
          return Response.json({
            output: [
              {
                content: [
                  {
                    text: JSON.stringify({
                      // Model the production false positive: an ordinary
                      // follow-up is classified as a different harness.
                      harness: strategyRequestCount === 1 ? 'codex' : null,
                      model: strategyRequestCount === 1 ? 'gpt-5.6-sol' : null,
                      provider: strategyRequestCount === 1 ? 'responses' : null,
                      reasoning: 'max'
                    })
                  }
                ]
              }
            ]
          })
        }) as unknown as typeof fetch,
        model: 'gpt-5.4-nano'
      }),
      state: sharedState
    })

    const sendMention = async (text: string, threadTs?: string) => {
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs })
      expect(mention.response.status).toBe(200)
      await mention.finished()
      return mention
    }

    const nanocodexRoot = await sendMention('--nanocodex --subagents start with the native harness')
    await sendMention('keep thinking without another flag', nanocodexRoot.ts)
    const defaultRoot = await sendMention('use the configured default model')

    expect(codexApi.creates.map(create => create.body.harness_type)).toEqual([
      'nanocodex',
      'nanocodex',
      'claudecode'
    ])
    expect(codexApi.creates[0]!.body.on_harness_conflict).toBe('restart')
    expect(codexApi.creates[2]!.body.on_harness_conflict).toBeUndefined()
    // The explicit flag bypasses the deployed LLM strategy. Only the two
    // unflagged messages consult it.
    expect(strategyRequestCount).toBe(2)
    expect(JSON.stringify(codexApi.executes[0]!.body)).not.toContain('--nanocodex')
    expect(JSON.stringify(codexApi.executes[0]!.body)).toContain('--subagents')
    expect(JSON.stringify(codexApi.executes[0]!.body)).toContain(
      'start with the native harness'
    )
    const followUpInput = JSON.parse(
      codexApi.executes[1]!.body.input_lines.at(-1)!
    ) as Record<string, unknown>
    expect(followUpInput.model).toBeUndefined()
    expect(followUpInput.provider).toBeUndefined()
    // Nanocodex supports Max on its default GPT-5.6 Sol model.
    expect(followUpInput.reasoning).toBe('max')
    const defaultInput = JSON.parse(
      codexApi.executes[2]!.body.input_lines.at(-1)!
    ) as Record<string, unknown>
    // The default Claude model also supports Max, so the inferred effort is
    // forwarded; the harness applies it to this turn only.
    expect(defaultInput.reasoning).toBe('max')

    const nanocodexState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${threadKey(nanocodexRoot.ts)}`
    )
    const defaultState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${threadKey(defaultRoot.ts)}`
    )
    expect(nanocodexState?.harnessType).toBe('nanocodex')
    expect(defaultState?.harnessType).toBeUndefined()
  })

  it('appends response metadata to every assistant message', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ responseMetadataMode: 'always', state: sharedState })

    const metadataBlockTexts = (calls: StreamCall[]): string[] =>
      calls
        .filter(call => call.method === 'chat.stopStream')
        .flatMap(call => (Array.isArray(call.body.blocks) ? (call.body.blocks as unknown[]) : []))
        .map(block => JSON.stringify(block))
        .filter(text => text.includes('Sol 5.6'))

    const parent = await postUserMessage('Response metadata thread context.')
    const firstMention = await sendMessage(`<@${BOT_USER_ID}> start`, { threadTs: parent.ts })
    expect(firstMention.response.status).toBe(200)
    await firstMention.finished()
    expect(metadataBlockTexts(slackApi.calls)).toHaveLength(1)
    expect(metadataBlockTexts(slackApi.calls)[0]).toContain('Codex')
    expect(metadataBlockTexts(slackApi.calls)[0]).toContain('Medium')
    expect(metadataBlockTexts(slackApi.calls)[0]).not.toContain('Fast')

    slackApi.reset()
    const secondMention = await sendMessage(`<@${BOT_USER_ID}> continue`, { threadTs: parent.ts })
    expect(secondMention.response.status).toBe(200)
    await secondMention.finished()
    expect(metadataBlockTexts(slackApi.calls)).toHaveLength(1)
  })

  it('adds the service tier without showing metadata on every response', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({
      responseServiceTierEnabled: true,
      state: sharedState
    })

    const metadataBlockTexts = (calls: StreamCall[]): string[] =>
      calls
        .filter(call => call.method === 'chat.stopStream')
        .flatMap(call => (Array.isArray(call.body.blocks) ? (call.body.blocks as unknown[]) : []))
        .map(block => JSON.stringify(block))
        .filter(text => text.includes('Sol 5.6'))

    const parent = await postUserMessage('Service tier thread context.')
    const firstMention = await sendMessage(`<@${BOT_USER_ID}> start`, { threadTs: parent.ts })
    expect(firstMention.response.status).toBe(200)
    await firstMention.finished()
    expect(metadataBlockTexts(slackApi.calls)).toHaveLength(1)
    expect(metadataBlockTexts(slackApi.calls)[0]).toContain('Fast')

    slackApi.reset()
    const secondMention = await sendMessage(`<@${BOT_USER_ID}> continue`, { threadTs: parent.ts })
    expect(secondMention.response.status).toBe(200)
    await secondMention.finished()
    expect(metadataBlockTexts(slackApi.calls)).toHaveLength(0)
  })

  it('shows the Slack-assigned harness and retains rollout metadata', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    const harnessAssignment = {
      experiment: 'codex_nanocodex_ab',
      requested_harness: 'codex',
      cohort: 'nanocodex',
      rollout_percent: 100
    }
    bot = createTestBot({
      codexNanocodexRolloutPercent: 100,
      state: sharedState
    })

    const parent = await postUserMessage('A/B test thread context.')
    const mention = await sendMessage(
      `<@${BOT_USER_ID}> inspect the repository and report what you find`,
      { threadTs: parent.ts }
    )
    expect(mention.response.status).toBe(200)
    await mention.finished()

    const footer = slackApi.calls
      .filter(call => call.method === 'chat.stopStream')
      .flatMap(call => (Array.isArray(call.body.blocks) ? (call.body.blocks as unknown[]) : []))
      .map(block => JSON.stringify(block))
      .find(text => text.includes('Nanocodex'))
    expect(footer).toContain('Nanocodex')
    expect(footer).toContain('Medium')
    expect(footer).not.toContain('Codex*')
    expect(codexApi.creates[0]?.body.harness_type).toBe('nanocodex')
    expect(codexApi.creates[0]?.body.metadata.harness_assignment).toEqual(harnessAssignment)
    expect(codexApi.executes[0]?.body.metadata).toMatchObject({
      harness_type: 'nanocodex',
      harness_assignment: harnessAssignment
    })
  })

  it('shows the harness default model in response metadata when no --model is set', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })

    const metadataBlockTexts = (calls: StreamCall[]): string[] =>
      calls
        .filter(call => call.method === 'chat.stopStream')
        .flatMap(call => (Array.isArray(call.body.blocks) ? (call.body.blocks as unknown[]) : []))
        .map(block => JSON.stringify(block))
        .filter(text => text.includes('Claude Code'))

    const parent = await postUserMessage('Default model thread context.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> --claude what is your current model?`, {
      threadTs: parent.ts
    })
    expect(mention.response.status).toBe(200)
    await mention.finished()

    const blocks = metadataBlockTexts(slackApi.calls)
    expect(blocks).toHaveLength(1)
    expect(blocks[0]).toContain('Claude Code')
    expect(blocks[0]).toContain(modelDisplayName(claudeSettings.model))

    // The effective default model is recorded in execution metadata, but never
    // forwarded to the harness — only explicit overrides ride the input lines.
    expect(codexApi.executes).toHaveLength(1)
    const executeBody = codexApi.executes[0]!.body
    expect(executeBody.metadata.model).toBe(claudeSettings.model)
    expect(JSON.parse(executeBody.input_lines.at(-1)!).model).toBeUndefined()
  })

  it('drops a per-channel reasoning default incompatible with its selected model', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({
      state: sharedState,
      // The channel pins Claude and also carries a Codex-only effort.
      channelDefaults: {
        [CHANNEL_ID]: { harnessType: 'claudecode', model: 'claude-opus-4-8', reasoning: 'minimal' }
      }
    })

    const parent = await postUserMessage('Channel default thread context.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> investigate this`, { threadTs: parent.ts })
    expect(mention.response.status).toBe(200)
    await mention.finished()

    // No explicit flags, but the channel default selects the harness and rides
    // the model/reasoning onto the input line (unlike the deployment/baked
    // default) and is recorded in execution metadata.
    expect(codexApi.creates.map(create => create.body.harness_type)).toEqual(['claudecode'])
    expect(codexApi.executes).toHaveLength(1)
    const executeBody = codexApi.executes[0]!.body
    const inputLine = JSON.parse(executeBody.input_lines.at(-1)!) as Record<string, unknown>
    expect(inputLine.model).toBe('claude-opus-4-8')
    expect(inputLine.reasoning).toBeUndefined()
    expect(executeBody.metadata.model).toBe('claude-opus-4-8')
  })

  it('materializes Slack event files on root mentions without fetching thread replies', async () => {
    const mention = await postUserMessage(`<@${BOT_USER_ID}> inspect this root screenshot`)
    const fileUrl = 'https://files.slack.com/files/captured.png'
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      text: `<@${BOT_USER_ID}> inspect this root screenshot`,
      files: [
        {
          id: 'F-root-captured',
          mimetype: 'image/png',
          name: 'captured.png',
          original_h: 600,
          original_w: 800,
          size: 16,
          url_private: fileUrl
        }
      ]
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()

    const appendedAttachment = codexApi.appends[0]!.body.messages
      .flatMap(message => message.parts)
      .find(part => isRecord(part) && part.type === 'attachment')
    expect(appendedAttachment).toEqual(
      expect.objectContaining({
        attachment_type: 'image',
        dataBase64: Buffer.from('captured-image').toString('base64'),
        mimeType: 'image/png',
        name: 'captured.png',
        type: 'attachment',
        url: fileUrl
      })
    )

    const executeInput = JSON.stringify(JSON.parse(codexApi.executes[0]!.body.input_lines[0]!))
    expect(executeInput).toContain(`"dataBase64":"${Buffer.from('captured-image').toString('base64')}"`)
    expect(executeInput).toContain('"attachment_type":"image"')
  })

  it('repairs delayed Slack Connect file-only messages as a follow-up turn', async () => {
    const mention = await sendMessage(`<@${BOT_USER_ID}> what's in this image`)
    expect(mention.response.status).toBe(200)
    await mention.finished()

    const fileUrl = `${slackApi.url}/files/captured.png`
    const fileTs = incrementSlackTs(mention.ts, 2)
    const fileDelivery = await deliverEvent({
      type: 'message',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: fileTs,
      text: '',
      files: [
        {
          id: 'F-late-captured',
          mimetype: 'image/png',
          name: 'late-captured.png',
          original_h: 600,
          original_w: 800,
          size: 16,
          url_private: fileUrl
        }
      ]
    })

    expect(fileDelivery.response.status).toBe(200)
    await fileDelivery.finished()

    expect(codexApi.executes).toHaveLength(2)
    expect(codexApi.executes.map(execute => execute.threadKey)).toEqual([
      threadKey(mention.ts),
      threadKey(mention.ts)
    ])
    expect(codexApi.executes[1]!.body.idempotency_key).toBe(fileTs)
    const appendedAttachment = codexApi.appends[1]!.body.messages
      .flatMap(message => message.parts)
      .find(part => isRecord(part) && part.type === 'attachment')
    expect(appendedAttachment).toEqual(
      expect.objectContaining({
        attachment_type: 'image',
        dataBase64: Buffer.from('captured-image').toString('base64'),
        mimeType: 'image/png',
        name: 'late-captured.png',
        type: 'attachment',
        url: fileUrl
      })
    )
    const secondExecuteInput = JSON.stringify(
      JSON.parse(codexApi.executes[1]!.body.input_lines.at(-1)!)
    )
    expect(secondExecuteInput).toContain('Late Slack file attachment')
    expect(secondExecuteInput).toContain('"attachment_type":"image"')
  })

  it('hydrates Slack Connect check_file_info placeholders before late-file repair', async () => {
    const mention = await sendMessage(`<@${BOT_USER_ID}> inspect the delayed file`)
    await mention.finished()

    const fileUrl = `${slackApi.url}/files/captured.png`
    slackApi.setFileInfo('F-check-file-info', {
      id: 'F-check-file-info',
      mimetype: 'image/png',
      name: 'hydrated.png',
      original_h: 600,
      original_w: 800,
      size: 16,
      url_private: fileUrl
    })

    const fileTs = incrementSlackTs(mention.ts, 2)
    const fileDelivery = await deliverEvent({
      type: 'message',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: fileTs,
      text: '',
      files: [{ id: 'F-check-file-info', file_access: 'check_file_info' }]
    })

    expect(fileDelivery.response.status).toBe(200)
    await fileDelivery.finished()

    expect(slackApi.fileInfoRequestCount('F-check-file-info')).toBe(1)
    expect(codexApi.executes).toHaveLength(2)
    const appendedAttachment = codexApi.appends[1]!.body.messages
      .flatMap(message => message.parts)
      .find(part => isRecord(part) && part.type === 'attachment')
    expect(appendedAttachment).toEqual(
      expect.objectContaining({
        dataBase64: Buffer.from('captured-image').toString('base64'),
        mimeType: 'image/png',
        name: 'hydrated.png',
        type: 'attachment',
        url: fileUrl
      })
    )
  })

  it('skips late-file repair when the webhook is not accepted', async () => {
    const mention = await sendMessage(`<@${BOT_USER_ID}> a file may follow`)
    await mention.finished()

    const fileEvent = signedSlackEvent({
      event_id: 'Ev-slackbotv2-late-file-skip',
      event: {
        type: 'message',
        user: USER_ID,
        channel: CHANNEL_ID,
        team: TEAM_ID,
        ts: incrementSlackTs(mention.ts, 2),
        text: '',
        files: [{ id: 'F-late-skip', file_access: 'check_file_info' }]
      }
    })
    const skipWaits: Promise<unknown>[] = []
    const skipResponse = await bot.app.request(
      '/api/webhooks/slack',
      {
        ...fileEvent,
        headers: { ...(fileEvent.headers as Record<string, string>), 'x-slack-signature': 'v0=0' }
      },
      {},
      waitUntilContext(skipWaits)
    )
    await Promise.all(skipWaits)

    expect(skipResponse.status).toBe(401)
    expect(slackApi.fileInfoRequestCount('F-late-skip')).toBe(0)
    expect(codexApi.executes).toHaveLength(1)
  })

  it('ignores unmatched and duplicate delayed file-only messages', async () => {
    const mention = await sendMessage(`<@${BOT_USER_ID}> maybe an image follows`)
    await mention.finished()

    const unrelatedDelivery = await deliverEvent({
      type: 'message',
      user: USER_B_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: incrementSlackTs(mention.ts, 2),
      text: '',
      files: [{ id: 'F-unmatched', mimetype: 'image/png', name: 'unmatched.png' }]
    })
    await unrelatedDelivery.finished()
    expect(codexApi.executes).toHaveLength(1)

    const fileEvent = signedSlackEvent({
      event_id: 'Ev-slackbotv2-duplicate-late-file',
      event: {
        type: 'message',
        user: USER_ID,
        channel: CHANNEL_ID,
        team: TEAM_ID,
        ts: incrementSlackTs(mention.ts, 3),
        text: '',
        files: [
          {
            id: 'F-dedupe-late',
            mimetype: 'image/png',
            name: 'dedupe.png',
            url_private: `${slackApi.url}/files/captured.png`
          }
        ]
      }
    })
    const firstWaits: Promise<unknown>[] = []
    await bot.app.request('/api/webhooks/slack', fileEvent, {}, waitUntilContext(firstWaits))
    await Promise.all(firstWaits)
    const duplicateWaits: Promise<unknown>[] = []
    await bot.app.request('/api/webhooks/slack', fileEvent, {}, waitUntilContext(duplicateWaits))
    await Promise.all(duplicateWaits)

    expect(codexApi.executes).toHaveLength(2)
    expect(codexApi.executes[1]!.body.idempotency_key).toBe(incrementSlackTs(mention.ts, 3))
  })

  it('fetches attachments from preceding Slack thread messages for a mid-thread mention', async () => {
    const parent = await postUserMessage('Root context before an attachment.')
    const priorReply = await postUserMessage('Screenshot is attached here.', parent.ts)
    const fileUrl = `${slackApi.url}/files/captured.png`
    slackApi.addFileToMessage(CHANNEL_ID, priorReply.ts, {
      id: 'F-thread-context-image',
      mimetype: 'image/png',
      name: 'thread-context.png',
      original_h: 600,
      original_w: 800,
      size: 16,
      url_private: fileUrl
    })
    const mention = await sendMessage(`<@${BOT_USER_ID}> inspect the earlier screenshot`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await mention.finished()

    const appendedAttachment = codexApi.appends[0]!.body.messages
      .flatMap(message => message.parts)
      .find(part => isRecord(part) && part.type === 'attachment')
    expect(appendedAttachment).toEqual(
      expect.objectContaining({
        attachment_type: 'image',
        dataBase64: Buffer.from('captured-image').toString('base64'),
        mimeType: 'image/png',
        name: 'thread-context.png',
        type: 'attachment',
        url: fileUrl
      })
    )

    const executeInput = JSON.stringify(JSON.parse(codexApi.executes[0]!.body.input_lines.at(-1)!))
    expect(executeInput).toContain('Screenshot is attached here.')
    expect(executeInput).toContain('Earlier Slack thread attachment')
    expect(executeInput).toContain('"attachment_type":"image"')
    expect(executeInput).toContain('"type":"attachment"')
    expect(executeInput).toContain(`"dataBase64":"${Buffer.from('captured-image').toString('base64')}"`)
    expect(executeInput).not.toContain('data:image/png;base64')
  })

  it('caches Slack requester identity across mentions from the same user', async () => {
    slackApi.setUserProfile(USER_ID, {
      name: 'akshaan',
      real_name: 'Akshaan Kakar',
      fields: {
        X_GITHUB: {
          label: 'GitHub',
          value: 'https://github.com/decofe'
        }
      }
    })

    for (const index of [1, 2]) {
      const mention = await sendMessage(`<@${BOT_USER_ID}> identity cache ${index}`)
      expect(mention.response.status).toBe(200)
      await mention.finished()
    }

    expect(codexApi.executes).toHaveLength(2)
    expect(slackApi.userProfileMethodRequestCount(USER_ID, '/api/users.profile.get')).toBe(1)
    for (const execute of codexApi.executes) {
      const input = JSON.parse(execute.body.input_lines.at(-1)!) as {
        message: { content: Array<{ text?: string; type: string }> }
      }
      expect(contentTextWithHeading(input.message.content, '# Requester Context')).toContain(
        'GitHub handle from Slack profile: @decofe'
      )
    }
  })

  it('stages large Slack file attachments without exceeding session input line limits', async () => {
    const parent = await postUserMessage('Context before the video upload.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> inspect this mp4`, parent.ts)
    const fileUrl = 'https://files.slack.com/files/large-upload.mp4'
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      thread_ts: parent.ts,
      text: `<@${BOT_USER_ID}> inspect this mp4`,
      files: [
        {
          id: 'F-large-mp4',
          mimetype: 'video/mp4',
          name: 'large-upload.mp4',
          size: 2 * 1024 * 1024,
          url_private: fileUrl
        }
      ]
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()

    const appendedAttachment = codexApi.appends[0]!.body.messages
      .flatMap(message => message.parts)
      .find(part => isRecord(part) && part.type === 'attachment')
    expect(appendedAttachment).toEqual(
      expect.objectContaining({
        attachment_type: 'video',
        dataBase64Omitted: expect.stringContaining('base64 chars omitted'),
        mimeType: 'video/mp4',
        name: 'large-upload.mp4'
      })
    )
    expect(appendedAttachment).not.toHaveProperty('dataBase64')

    const inputLines = codexApi.executes[0]!.body.input_lines
    expect(inputLines.length).toBeGreaterThan(1)
    for (const line of inputLines) {
      expect(line.length).toBeLessThanOrEqual(1048576)
    }

    const chunkInputs = inputLines.slice(0, -1).map(line => JSON.parse(line))
    expect(chunkInputs.every(input => input.type === 'attachment.chunk')).toBe(true)
    expect(chunkInputs.at(-1)).toEqual(expect.objectContaining({ final: true }))

    const turnInput = JSON.parse(inputLines.at(-1)!) as Record<string, unknown>
    const serializedTurn = JSON.stringify(turnInput)
    expect(serializedTurn).toContain('"stagedAttachmentId"')
    expect(serializedTurn).not.toContain('dataBase64')
  })

  it('executes a root app mention without channel history', async () => {
    await postUserMessage('Prior channel message A.')
    await postUserMessage('Prior channel message B.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> answer from a new root message`)
    slackApi.failRepliesWithThreadNotFound(CHANNEL_ID, mention.ts)
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      text: `<@${BOT_USER_ID}> answer from a new root message`
    })

    expect(delivery.response.status).toBe(200)
    await delivery.finished()

    expect(codexApi.creates.map(create => create.threadKey)).toEqual([threadKey(mention.ts)])
    expect(codexApi.appends).toHaveLength(1)
    expect(sessionMessageTexts(codexApi.appends[0]!.body.messages)).toEqual([
      '@centaur answer from a new root message'
    ])
    expect(codexApi.executes).toHaveLength(1)
    expect(JSON.stringify(JSON.parse(codexApi.executes[0]!.body.input_lines[0]!))).toContain(
      'answer from a new root message'
    )
    expectSlackPlanStreamShape(slackApi.calls, {
      answers: ['Executed request 1.'],
      parentTs: mention.ts
    })
  })

  it('ignores non-JSON sandbox bootstrap output lines instead of ending the stream', async () => {
    codexApi.autoRespond = false

    const mention = await sendMessage(`<@${BOT_USER_ID}> answer after bootstrap noise`)
    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)

    const key = threadKey(mention.ts)
    codexApi.emitOutputLine(key, 'installed 62 Centaur tool CLI shims into /home/agent/.local/bin')
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Answer despite bootstrap noise.'))

    await mention.finished()
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(true)
    expect(await threadText(mention.ts)).toContain('Answer despite bootstrap noise.')
    expect(await threadText(mention.ts)).not.toContain(
      'Execution completed, but no final text was captured.'
    )
  })

  it('auto-disables steering reactions when reactions:write is missing', async () => {
    const logs: CapturedLog[] = []
    bot = createTestBot({
      logger: captureLogger(logs),
      steeringReactionEnabled: true
    })
    slackApi.respondToNextReaction(200, {
      ok: false,
      error: 'missing_scope',
      needed: 'reactions:write'
    })
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before missing reaction scope.')
    const firstMention = await sendMessage(`<@${BOT_USER_ID}> start running`, {
      threadTs: parent.ts
    })
    expect(firstMention.response.status).toBe(200)
    await waitFor(() => codexApi.streamCount === 1)

    for (const [index, text] of ['first steer', 'second steer'].entries()) {
      const mention = await sendMessage(`<@${BOT_USER_ID}> ${text}`, { threadTs: parent.ts })
      expect(mention.response.status).toBe(200)
      await mention.finished()
    }

    expect(slackApi.calls.filter(call => call.method === 'reactions.add')).toHaveLength(1)
    expect(slackApi.calls.some(call => call.method === 'reactions.remove')).toBe(false)
    expect(logData(logs, 'slackbotv2_steering_reaction_auto_disabled')).toEqual(
      expect.objectContaining({
        error: 'missing_scope',
        needed: 'reactions:write',
        operation: 'add'
      })
    )
    expect(codexApi.appends).toHaveLength(3)
    expect(codexApi.executes).toHaveLength(1)

    codexApi.closeStreams()
    await firstMention.finished()
  })

  it('renders raw turn.failed session output as visible final text', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a raw failure.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run a failing turn`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'gh auth status',
          status: 'inProgress'
        }
      })
    )
    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'turn.failed',
        error: {
          message: 'Reconnecting... 2/5',
          additionalDetails: 'unexpected status 502 Bad Gateway'
        }
      })
    )

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const markdownChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'markdown_text')
    expect(markdownChunks).toEqual([
      {
        type: 'markdown_text',
        text: 'Execution failed: Reconnecting... 2/5: unexpected status 502 Bad Gateway'
      }
    ])
    const renderedText = transcripts[0]!.chunks.map(chunkText).filter(Boolean).join('\n')
    expect(renderedText).toContain('Command execution')
    expect(renderedText).toContain(
      'Execution failed: Reconnecting... 2/5: unexpected status 502 Bad Gateway'
    )
  })

  it('renders interrupted executions with no final answer as interrupted', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before an interrupt.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run until stopped`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'sleep 60',
          status: 'inProgress'
        }
      })
    )
    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'sleep 60',
          status: 'failed',
          aggregatedOutput: '',
          exitCode: 130
        }
      })
    )
    codexApi.emitSessionEvent(threadKey(parent.ts), 'session.execution_cancelled', {
      execution_id: 'exe-interrupted',
      status: 'cancelled',
      reason: 'turn_interrupted'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const markdownChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'markdown_text')
    expect(markdownChunks).toEqual([
      {
        type: 'markdown_text',
        text: 'Execution interrupted'
      }
    ])
    const renderedText = transcripts[0]!.chunks.map(chunkText).filter(Boolean).join('\n')
    expect(renderedText).toContain('Command execution')
    expect(renderedText.trim().endsWith('Execution interrupted')).toBe(true)
    expect(renderedText).not.toContain('Execution completed, but no final text was captured.')
  })

  it('renders api-rs completion result text when no final answer delta streamed', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a terminal completion.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> complete from terminal payload`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'true',
          status: 'inProgress'
        }
      })
    )
    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'true',
          status: 'completed',
          aggregatedOutput: ''
        }
      })
    )
    codexApi.emitSessionEvent(threadKey(parent.ts), 'session.execution_completed', {
      execution_id: 'exe-terminal-result',
      status: 'completed',
      result_text: 'TERMINAL_RESULT_VISIBLE'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const renderedText = transcripts[0]!.chunks.map(chunkText).filter(Boolean).join('\n')
    expect(renderedText).toContain('Command execution')
    expect(renderedText).toContain('TERMINAL_RESULT_VISIBLE')
    expect(renderedText).not.toContain('Execution completed, but no final text was captured.')
  })

  it('replaces the failed stream with the durable final answer when Slack rejects stop as too long', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false
    // Every stop fails: the streamed message never finalizes, so its content
    // breaks in real Slack. Size-limit failures should be prevented by
    // segmentation; if one still happens, replace the broken stream instead of
    // posting a duplicate fallback reply in the thread.
    slackApi.failStreamStopsLongerThan(10)

    const parent = await postUserMessage('Context before an oversized Slack render.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> generate noisy progress`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-oversized',
          type: 'commandExecution',
          command: 'printf noisy',
          status: 'completed',
          aggregatedOutput: 'x'.repeat(20_000)
        }
      })
    )
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-msg-too-long-fallback',
      status: 'completed',
      result_text: 'TOO_LONG_FALLBACK_VISIBLE'
    })

    await mention.finished()
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(true)
    const texts = await threadTexts(parent.ts)
    expect(texts.some(text => text.includes(BROKEN_STREAM_TEXT))).toBe(false)
    expect(texts.filter(text =>
      text.includes('TOO_LONG_FALLBACK_VISIBLE')
    )).toHaveLength(1)
    const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
    expect(threadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        renderObligation: null
      })
    )
  })

  it('finishes the render obligation without a duplicate reply when a failed stop succeeds on cleanup', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false
    slackApi.failNextStreamStop()

    const parent = await postUserMessage('Context before an transient stop failure.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> finish the answer`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-stop',
          type: 'commandExecution',
          command: 'printf noisy',
          status: 'completed',
          aggregatedOutput: 'done'
        }
      })
    )
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-transient-stop',
      status: 'completed',
      result_text: 'TRANSIENT_STOP_ANSWER_VISIBLE'
    })

    await mention.finished()
    expect(slackApi.calls.filter(call => call.method === 'chat.stopStream')).toHaveLength(2)
    // Recovery was confined to stop(): durable fallback would open a second SSE request.
    expect(codexApi.eventRequests).toHaveLength(1)
    const texts = await threadTexts(parent.ts)
    expect(texts.some(text => text.includes(BROKEN_STREAM_TEXT))).toBe(false)
    expect(texts.filter(text =>
      text.includes('TRANSIENT_STOP_ANSWER_VISIBLE')
    )).toHaveLength(1)
    const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
    expect(threadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        renderObligation: null
      })
    )
  })

  it('swaps the streamed message for the durable final answer when the live answer diverges', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a diverging render.')
    const mentionText = `<@${BOT_USER_ID}> answer with a late correction`
    const mention = await postUserMessage(mentionText, parent.ts)
    const key = threadKey(parent.ts)
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      thread_ts: parent.ts,
      text: mentionText
    })
    expect(delivery.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const draft = 'Draft answer from the live deltas.'
    const finalAnswer = `**Final reconciled answer from the result.**\n${'@tester '.repeat(25)}${'x'.repeat(13_000)}`
    // Stream a plan + the draft answer (so the answer delta reaches Slack), then
    // seal the answer item with a DIFFERENT canonical text. The recomposed
    // answer no longer extends the already-streamed text, so the renderer
    // freezes the live stream instead of interleaving, and the render swaps the
    // message for the durable result.
    codexApi.emitOutputLines(
      key,
      sampleCodexNotifications(draft).map(notification => JSON.stringify(notification))
    )
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        method: 'item/completed',
        params: {
          threadId: 'thread-1',
          turnId: 'turn-1',
          item: {
            type: 'agentMessage',
            id: 'answer-1',
            text: finalAnswer,
            phase: 'final_answer',
            memoryCitation: null
          }
        }
      })
    )
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-divergence-swap',
      status: 'completed',
      result_text: finalAnswer
    })

    await delivery.finished()
    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return threadState?.renderObligation === null
    }, 3000)

    const texts = await threadTexts(parent.ts)
    // The streamed message was replaced in place with the durable final answer...
    expect(texts.filter(text => text.includes('Final reconciled answer from the result.'))).toHaveLength(1)
    // ...and the diverging live draft is gone (neither interleaved nor left behind).
    expect(texts.some(text => text.includes('Draft answer from the live deltas'))).toBe(false)
    const replacementUpdate = slackApi.calls.find(call => call.method === 'chat.update')
    expect(replacementUpdate).toBeDefined()
    expect(replacementUpdate?.body.text).toBeUndefined()
    expect(stringField(replacementUpdate?.body.markdown_text)).toStartWith(
      '**Final reconciled answer from the result.**'
    )
    expect(stringField(replacementUpdate?.body.markdown_text).length).toBeLessThanOrEqual(
      SLACK_MARKDOWN_TEXT_MAX_CHARS
    )
    expect(stringField(replacementUpdate?.body.markdown_text)).toContain('[truncated ')
  })

  it('reposts the durable final answer when the Slack stream expires mid-render', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false
    // The first append succeeds, then Slack expires the streaming message
    // (production: ~300s after chat.startStream) and every further append
    // fails. The final answer has not reached Slack at that point.
    slackApi.failStreamAppendsAfter(1, 'message_not_in_streaming_state')

    const parent = await postUserMessage('Context before a stream expiry.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run something long`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-long',
          type: 'commandExecution',
          command: 'sleep 600',
          status: 'completed',
          aggregatedOutput: 'done'
        }
      })
    )
    const finalAnswer = `**EXPIRED_STREAM_FALLBACK_VISIBLE**\n${'@tester '.repeat(25)}${'x'.repeat(13_000)}`
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-stream-expired',
      status: 'completed',
      result_text: finalAnswer
    })

    await mention.finished()
    const texts = await threadTexts(parent.ts)
    const visibleFinalReplies = texts.filter(text =>
      text.includes('EXPIRED_STREAM_FALLBACK_VISIBLE')
    )
    expect(visibleFinalReplies).toHaveLength(1)
    const fallbackPost = slackApi.calls.find(call => call.method === 'chat.postMessage')
    expect(fallbackPost).toBeDefined()
    expect(fallbackPost?.body.text).toBeUndefined()
    expect(stringField(fallbackPost?.body.markdown_text)).toStartWith(
      '**EXPIRED_STREAM_FALLBACK_VISIBLE**'
    )
    // The fallback budget leaves room for the adapter to expand bare mentions
    // without crossing Slack's 12,000-character markdown_text limit.
    expect(stringField(fallbackPost?.body.markdown_text).length).toBeLessThanOrEqual(
      SLACK_MARKDOWN_TEXT_MAX_CHARS
    )
    expect(stringField(fallbackPost?.body.markdown_text)).toContain('[truncated ')
    const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
    expect(threadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        renderObligation: null
      })
    )
  })

  it('rotates Slack stream segments before they reach the streaming age limit', async () => {
    process.env.SLACK_STREAM_SEGMENT_MAX_AGE_MS = '120'
    bot = createTestBot()
    try {
      codexApi.autoRespond = false

      const parent = await postUserMessage('Context before a slow render.')
      const mention = await sendMessage(`<@${BOT_USER_ID}> work slowly`, { threadTs: parent.ts })
      const key = threadKey(parent.ts)

      expect(mention.response.status).toBe(200)
      await waitFor(() => codexApi.executes.length === 1)
      await waitFor(() => codexApi.eventRequests.length === 1)
      await waitFor(() => codexApi.streamCount === 1)

      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: 'cmd-slow-1',
            type: 'commandExecution',
            // Large enough to flush the Slack SDK's client-side stream buffer
            // so the first segment demonstrably carries content.
            command: `sleep 1 # ${'x'.repeat(300)}`,
            status: 'completed',
            aggregatedOutput: 'first'
          }
        })
      )
      // Wait for the first segment to age past the rotation threshold, then
      // keep streaming: the adapter must continue in a fresh stream message.
      await waitFor(() => slackApi.calls.some(call => call.method === 'chat.startStream'))
      await new Promise(resolve => setTimeout(resolve, 250))
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: 'cmd-slow-2',
            type: 'commandExecution',
            command: 'sleep 2',
            status: 'completed',
            aggregatedOutput: 'second'
          }
        })
      )
      codexApi.emitSessionEvent(key, 'session.execution_completed', {
        execution_id: 'exe-age-rotation',
        status: 'completed',
        result_text: 'AGE_ROTATION_ANSWER_VISIBLE'
      })

      await mention.finished()
      const starts = slackApi.calls.filter(call => call.method === 'chat.startStream')
      expect(starts.length).toBeGreaterThanOrEqual(2)
      const startTs = new Set(
        starts.map(call => call.streamTs).filter((ts): ts is string => Boolean(ts))
      )
      const stopTs = new Set(
        slackApi.calls
          .filter(call => call.method === 'chat.stopStream')
          .map(call => stringField(call.body.ts))
      )
      for (const ts of startTs) {
        expect(stopTs.has(ts)).toBe(true)
      }
      const texts = await threadTexts(parent.ts)
      expect(texts.some(text => text.includes(BROKEN_STREAM_TEXT))).toBe(false)
      const visibleFinalReplies = texts.filter(text =>
        text.includes('AGE_ROTATION_ANSWER_VISIBLE')
      )
      expect(visibleFinalReplies).toHaveLength(1)
    } finally {
      delete process.env.SLACK_STREAM_SEGMENT_MAX_AGE_MS
    }
  })

  it('marks open tasks complete before rotating an aged progress segment', async () => {
    process.env.SLACK_STREAM_SEGMENT_MAX_AGE_MS = '120'
    bot = createTestBot()
    try {
      codexApi.autoRespond = false

      const parent = await postUserMessage('Context before an aged open task.')
      const mention = await sendMessage(`<@${BOT_USER_ID}> keep thinking`, { threadTs: parent.ts })
      const key = threadKey(parent.ts)

      expect(mention.response.status).toBe(200)
      await waitFor(() => codexApi.executes.length === 1)
      await waitFor(() => codexApi.eventRequests.length === 1)
      await waitFor(() => codexApi.streamCount === 1)

      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.started',
          item: {
            id: 'cmd-aging-open',
            type: 'commandExecution',
            command: `sleep 1 # ${'x'.repeat(300)}`,
            status: 'inProgress'
          }
        })
      )
      await waitFor(() =>
        slackApi.calls.some(call =>
          streamChunks(call.body.chunks).some(
            chunk => chunk.id === 'cmd-aging-open' && chunk.status === 'in_progress'
          )
        )
      )

      await new Promise(resolve => setTimeout(resolve, 250))
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: 'cmd-aging-open',
            type: 'commandExecution',
            command: 'sleep 1',
            status: 'completed',
            aggregatedOutput: ''
          }
        })
      )
      codexApi.emitSessionEvent(key, 'session.execution_completed', {
        execution_id: 'exe-open-task-age-rotation',
        status: 'completed',
        result_text: 'OPEN_TASK_AGE_ROTATION_OK'
      })

      await mention.finished()
      const transcripts = slackStreamTranscripts(slackApi.calls)
      expect(transcripts.length).toBeGreaterThan(1)
      const taskTranscripts = transcripts.filter(transcript =>
        transcript.chunks.some(chunk => chunk.type === 'task_update' && chunk.id === 'cmd-aging-open')
      )
      expect(taskTranscripts.length).toBeGreaterThan(0)
      for (const transcript of taskTranscripts) {
        const statuses = transcript.chunks
          .filter(chunk => chunk.type === 'task_update' && chunk.id === 'cmd-aging-open')
          .map(chunk => stringField(chunk.status))
        expect(statuses[statuses.length - 1]).toBe('complete')
      }
      const texts = await threadTexts(parent.ts)
      expect(texts.some(text => text.includes(BROKEN_STREAM_TEXT))).toBe(false)
      expect(texts.filter(text => text.includes('OPEN_TASK_AGE_ROTATION_OK'))).toHaveLength(1)
    } finally {
      delete process.env.SLACK_STREAM_SEGMENT_MAX_AGE_MS
    }
  })

  it('rotates structured plan segments before they exceed the task char budget', async () => {
    process.env.SLACK_STREAM_SEGMENT_TASK_CHAR_BUDGET = '400'
    try {
      codexApi.autoRespond = false

      const parent = await postUserMessage('Context before a card-heavy render.')
      const mention = await sendMessage(`<@${BOT_USER_ID}> run many steps`, { threadTs: parent.ts })
      const key = threadKey(parent.ts)

      expect(mention.response.status).toBe(200)
      await waitFor(() => codexApi.executes.length === 1)
      await waitFor(() => codexApi.eventRequests.length === 1)
      await waitFor(() => codexApi.streamCount === 1)

      for (let index = 1; index <= 3; index++) {
        codexApi.emitOutputLine(
          key,
          JSON.stringify({
            type: 'item.completed',
            item: {
              id: `cmd-budget-${index}`,
              type: 'commandExecution',
              command: `step-${index} ${'x'.repeat(200)}`,
              status: 'completed',
              aggregatedOutput: 'ok'
            }
          })
        )
        // Let the renderer flush each card before emitting the next so the
        // budget accounting sees them as separate appends. The first flush of
        // a segment arrives as chat.startStream, later ones as appendStream.
        await waitFor(
          () =>
            slackApi.calls.filter(
              call => call.method === 'chat.appendStream' || call.method === 'chat.startStream'
            ).length >= index
        )
      }
      codexApi.emitSessionEvent(key, 'session.execution_completed', {
        execution_id: 'exe-budget-rotation',
        status: 'completed',
        result_text: 'BUDGET_ROTATION_ANSWER_VISIBLE'
      })

      await mention.finished()
      const starts = slackApi.calls.filter(call => call.method === 'chat.startStream')
      expect(starts.length).toBeGreaterThanOrEqual(2)
      const texts = await threadTexts(parent.ts)
      expect(texts.some(text => text.includes(BROKEN_STREAM_TEXT))).toBe(false)
      const visibleFinalReplies = texts.filter(text =>
        text.includes('BUDGET_ROTATION_ANSWER_VISIBLE')
      )
      expect(visibleFinalReplies).toHaveLength(1)
    } finally {
      delete process.env.SLACK_STREAM_SEGMENT_TASK_CHAR_BUDGET
    }
  })

  it('seals open tasks before stopping older structured progress segments', async () => {
    process.env.SLACK_STREAM_SEGMENT_TASK_CHAR_BUDGET = '520'
    try {
      codexApi.autoRespond = false

      const parent = await postUserMessage('Context before an open card spillover.')
      const mention = await sendMessage(`<@${BOT_USER_ID}> keep one step open`, {
        threadTs: parent.ts
      })
      const key = threadKey(parent.ts)

      expect(mention.response.status).toBe(200)
      await waitFor(() => codexApi.executes.length === 1)
      await waitFor(() => codexApi.eventRequests.length === 1)
      await waitFor(() => codexApi.streamCount === 1)

      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.started',
          item: {
            id: 'cmd-open-spillover',
            type: 'commandExecution',
            command: `sleep 1 # ${'x'.repeat(220)}`,
            status: 'inProgress'
          }
        })
      )
      await waitFor(() =>
        slackApi.calls.some(call =>
          streamChunks(call.body.chunks).some(
            chunk => chunk.id === 'cmd-open-spillover' && chunk.status === 'in_progress'
          )
        )
      )

      for (let index = 1; index <= 4; index += 1) {
        codexApi.emitOutputLine(
          key,
          JSON.stringify({
            type: 'item.completed',
            item: {
              id: `cmd-spillover-${index}`,
              type: 'commandExecution',
              command: `printf spillover-${index} ${'x'.repeat(220)}`,
              status: 'completed',
              aggregatedOutput: ''
            }
          })
        )
      }
      codexApi.emitSessionEvent(key, 'session.execution_completed', {
        execution_id: 'exe-open-task-structured-spillover',
        status: 'completed',
        result_text: 'OPEN_TASK_STRUCTURED_SPILLOVER_OK'
      })

      await mention.finished()
      const transcripts = slackStreamTranscripts(slackApi.calls)
      expect(transcripts.length).toBeGreaterThanOrEqual(2)
      const taskTranscripts = transcripts.filter(transcript =>
        transcript.chunks.some(
          chunk => chunk.type === 'task_update' && chunk.id === 'cmd-open-spillover'
        )
      )
      expect(taskTranscripts.length).toBeGreaterThan(0)
      for (const transcript of taskTranscripts) {
        const statuses = transcript.chunks
          .filter(chunk => chunk.type === 'task_update' && chunk.id === 'cmd-open-spillover')
          .map(chunk => stringField(chunk.status))
        expect(statuses).toContain('in_progress')
        expect(statuses[statuses.length - 1]).toBe('complete')
      }
      expect(await threadText(parent.ts)).toContain('OPEN_TASK_STRUCTURED_SPILLOVER_OK')
    } finally {
      delete process.env.SLACK_STREAM_SEGMENT_TASK_CHAR_BUDGET
    }
  })

  it('keeps card-heavy structured streams below Slack finalization payload limits', async () => {
    slackApi.failStreamStopsLongerThan(12_000)
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a production-sized card render.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run enough steps to paginate`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    for (let index = 1; index <= 36; index++) {
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: `cmd-payload-${index}`,
            type: 'commandExecution',
            command: `step-${index} ${'x'.repeat(220)}`,
            status: 'completed',
            aggregatedOutput: ''
          }
        })
      )
    }
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-structured-payload-budget',
      status: 'completed',
      result_text: 'STRUCTURED_PAYLOAD_BUDGET_ANSWER_VISIBLE'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts.length).toBeGreaterThanOrEqual(2)
    for (const transcript of transcripts) {
      expect(streamTranscriptPayloadChars(transcript)).toBeLessThanOrEqual(12_000)
    }
    const texts = await threadTexts(parent.ts)
    expect(texts.some(text => text.includes(BROKEN_STREAM_TEXT))).toBe(false)
    expect(texts.filter(text =>
      text.includes('STRUCTURED_PAYLOAD_BUDGET_ANSWER_VISIBLE')
    )).toHaveLength(1)
  })

  it('recovers the final answer when thread state already advanced past the terminal event', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()

    const parent = await postUserMessage('Context before restart recovery past terminal.')
    const mentionText = `<@${BOT_USER_ID}> recover a consumed run`
    const mention = await postUserMessage(mentionText, parent.ts)
    const key = threadKey(parent.ts)
    const message = apiMessageFromSlackEvent({
      isMention: true,
      text: mentionText,
      threadId: key,
      ts: mention.ts
    })
    // The crashed render consumed the whole stream (lastEventId advanced past
    // the terminal event) but the answer never reached Slack. Recovery must
    // replay from the obligation's starting position, not lastEventId.
    await sharedState.set(`thread-state:${key}`, {
      activeExecution: true,
      executedMessageIds: [mention.ts],
      forwardedMessageIds: [mention.ts],
      historyForwarded: true,
      lastEventId: 999999,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exe-recovery-consumed',
        message
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', key)
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Recovered consumed answer.'))

    bot = createTestBot({ state: sharedState })

    await waitFor(() => codexApi.eventRequests.length === 1, 2000)
    await waitFor(() => slackApi.calls.some(call => call.method === 'chat.stopStream'), 2000)

    expect(codexApi.eventRequests).toEqual([
      { afterEventId: 0, executionId: 'exe-recovery-consumed', threadKey: key }
    ])
    expect(await threadText(parent.ts)).toContain('Recovered consumed answer.')
    // Recovery clears the obligation after the Slack stream stops; wait for
    // the state write instead of racing it.
    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return threadState?.renderObligation === null
    }, 2000)
    const recoveredThreadState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${key}`
    )
    expect(recoveredThreadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        renderObligation: null
      })
    )
  })

  it('continues oversized final answers across Slack stream replies', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a long final answer.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> write a long visible answer`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const answer = `STREAM_CONTINUATION_START ${'x'.repeat(14_000)} STREAM_CONTINUATION_END`
    codexApi.emitOutputLines(key, sampleCodexOutputLines(answer))
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-stream-continuation',
      status: 'completed',
      result_text: answer
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts.length).toBeGreaterThan(1)
    expect(await threadText(parent.ts)).toContain('STREAM_CONTINUATION_START')
    expect(await threadText(parent.ts)).toContain('STREAM_CONTINUATION_END')
  })

  it('conflates rapid task updates instead of one Slack call per event', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a chatty command.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run the chatty command`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-chatty',
          type: 'commandExecution',
          command: 'stream-much-output',
          status: 'inProgress'
        }
      })
    )
    const updateCount = 400
    for (let index = 1; index <= updateCount; index += 1) {
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.commandExecution.outputDelta',
          itemId: 'cmd-chatty',
          delta: `line-${index}\n`
        })
      )
    }
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-chatty',
          type: 'commandExecution',
          command: 'stream-much-output',
          status: 'completed',
          aggregatedOutput: ''
        }
      })
    )
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-conflated-render',
      status: 'completed',
      result_text: 'CONFLATED_RENDER_OK'
    })

    await mention.finished()
    const chattyChunkSends = slackApi.calls.reduce((total, call) => {
      return (
        total +
        streamChunks(call.body.chunks).filter(chunk => chunk.id === 'cmd-chatty').length
      )
    }, 0)
    // Without conflation every output delta becomes its own Slack append
    // (~400 chunk sends for this card). Conflation folds updates that arrive
    // while a Slack call is in flight, so the card is sent far fewer times.
    expect(chattyChunkSends).toBeGreaterThan(0)
    expect(chattyChunkSends).toBeLessThan(100)
    const renderedText = slackStreamTranscripts(slackApi.calls)
      .flatMap(transcript => transcript.chunks.map(chunkText))
      .filter(Boolean)
      .join('\n')
    expect(renderedText).toContain('CONFLATED_RENDER_OK')
  })

  it('continues large task streams across Slack stream replies', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before many tool steps.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run many small steps`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    for (let index = 1; index <= 60; index += 1) {
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.started',
          item: {
            id: `cmd-${index}`,
            type: 'commandExecution',
            command: `printf step-${index}`,
            status: 'inProgress'
          }
        })
      )
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: `cmd-${index}`,
            type: 'commandExecution',
            command: `printf step-${index}`,
            status: 'completed',
            aggregatedOutput: ''
          }
        })
      )
    }
    codexApi.emitOutputLines(key, sampleCodexOutputLines('TASK_STREAM_CONTINUATION_OK'))
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-task-stream-continuation',
      status: 'completed',
      result_text: 'TASK_STREAM_CONTINUATION_OK'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts.length).toBeGreaterThan(1)
    expect(transcripts.flatMap(transcript => transcript.chunks).filter(chunk => chunk.type === 'task_update').length)
      .toBeGreaterThan(50)
    const taskCounts = transcripts.map(transcript =>
      new Set(
        transcript.chunks
          .filter(chunk => chunk.type === 'task_update')
          .map(chunk => stringField(chunk.id))
      ).size
    )
    expect(taskCounts[0]).toBeGreaterThan(0)
    expect(taskCounts[0]).toBeLessThan(50)
    expect(Math.max(...taskCounts)).toBeLessThanOrEqual(50)
    expect(await threadText(parent.ts)).toContain('TASK_STREAM_CONTINUATION_OK')
  })

  it('does not seal a Slack stream continuation while task cards are still open', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a task boundary page.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> run steps with one slow command`, {
      threadTs: parent.ts
    })
    const key = threadKey(parent.ts)

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    for (let index = 1; index <= 47; index += 1) {
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: `cmd-before-${index}`,
            type: 'commandExecution',
            command: `printf before-${index}`,
            status: 'completed',
            aggregatedOutput: ''
          }
        })
      )
    }
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-open',
          type: 'commandExecution',
          command: 'sleep 1 && true',
          status: 'inProgress'
        }
      })
    )
    // Conflation collapses unsent intermediate states, so wait until the open
    // task has actually reached Slack before completing it - this test is
    // about segments staying open while a card is in progress.
    await waitFor(() =>
      slackApi.calls.some(call =>
        streamChunks(call.body.chunks).some(
          chunk => chunk.id === 'cmd-open' && chunk.status === 'in_progress'
        )
      )
    )
    for (let index = 1; index <= 3; index += 1) {
      codexApi.emitOutputLine(
        key,
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: `cmd-during-${index}`,
            type: 'commandExecution',
            command: `printf during-${index}`,
            status: 'completed',
            aggregatedOutput: ''
          }
        })
      )
    }
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-open',
          type: 'commandExecution',
          command: 'sleep 1 && true',
          status: 'completed',
          aggregatedOutput: ''
        }
      })
    )
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.completed',
        item: {
          id: 'cmd-after-open',
          type: 'commandExecution',
          command: 'printf after-open',
          status: 'completed',
          aggregatedOutput: ''
        }
      })
    )
    codexApi.emitOutputLines(key, sampleCodexOutputLines('OPEN_TASK_PAGE_OK'))
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-open-task-page',
      status: 'completed',
      result_text: 'OPEN_TASK_PAGE_OK'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts.length).toBeGreaterThan(1)

    const transcriptWithOpenTask = transcripts.find(transcript =>
      transcript.chunks.some(chunk => chunk.type === 'task_update' && chunk.id === 'cmd-open')
    )
    expect(transcriptWithOpenTask).toBeDefined()
    expect(transcriptWithOpenTask!.chunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'cmd-open',
        status: 'in_progress'
      })
    )
    expect(transcriptWithOpenTask!.chunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'cmd-open',
        status: 'complete'
      })
    )
    expect(await threadText(parent.ts)).toContain('OPEN_TASK_PAGE_OK')
  })

  it('does not create an empty Slack stream before the first visible renderer chunk', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a silent execution.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> wait for actual output`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)
    await sleep(50)
    expect(slackApi.calls.some(call => call.method === 'chat.startStream')).toBe(false)

    codexApi.emitSessionEvent(threadKey(parent.ts), 'session.execution_completed', {
      execution_id: 'exe-delayed-visible-output',
      status: 'completed',
      result_text: 'DELAYED_VISIBLE_OUTPUT'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const renderedText = transcripts[0]!.chunks.map(chunkText).filter(Boolean).join('\n')
    expect(renderedText).toContain('DELAYED_VISIBLE_OUTPUT')
  })

  it('does not duplicate final text when execution completion follows final answer deltas', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a completion snapshot.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> guard against duplicate final text`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'answer-1',
          type: 'agentMessage',
          text: '',
          phase: 'final_answer'
        }
      })
    )
    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.agentMessage.delta',
        itemId: 'answer-1',
        delta: 'DUPLICATE_DELIVERY_GUARD_OK'
      })
    )
    codexApi.emitSessionEvent(threadKey(parent.ts), 'session.execution_completed', {
      execution_id: 'exe-duplicate-guard',
      status: 'completed'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const markdownChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'markdown_text')
    expect(markdownChunks).toEqual([
      {
        type: 'markdown_text',
        text: 'DUPLICATE_DELIVERY_GUARD_OK'
      }
    ])
    expect(
      transcripts[0]!.chunks.filter(chunk =>
        chunkText(chunk).includes('DUPLICATE_DELIVERY_GUARD_OK')
      )
    ).toHaveLength(1)
  })

  it('keeps each task on one bounded card as details grow and output expands', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before large task output.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> keep final text visible`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const largeOutput = 'large-context-line\n'.repeat(600)
    for (let index = 0; index < 6; index += 1) {
      codexApi.emitOutputLine(
        threadKey(parent.ts),
        JSON.stringify({
          type: 'item.started',
          item: {
            id: `cmd-large-${index}`,
            type: 'commandExecution',
            command: `slack thread --json --page ${index}`,
            status: 'inProgress'
          }
        })
      )
      codexApi.emitOutputLine(
        threadKey(parent.ts),
        JSON.stringify({
          type: 'item.completed',
          item: {
            id: `cmd-large-${index}`,
            type: 'commandExecution',
            command: `slack thread --json --page ${index}`,
            status: 'completed',
            aggregatedOutput: largeOutput
          }
        })
      )
    }
    codexApi.emitOutputLine(threadKey(parent.ts), JSON.stringify({
      type: 'item.started',
      item: { id: 'edited-files', type: 'fileChange', status: 'inProgress', changes: [{ path: 'first.ts' }] }
    }))
    await waitFor(() => slackApi.calls.some(call =>
      streamChunks(call.body.chunks).some(chunk => chunk.id === 'edited-files' && chunk.status === 'in_progress')
    ))
    codexApi.emitOutputLine(threadKey(parent.ts), JSON.stringify({
      type: 'item.completed',
      item: {
        id: 'edited-files', type: 'fileChange', status: 'completed',
        changes: Array.from({ length: 30 }, (_, index) => ({ path: `src/🙂-component-${index}.ts`, diff: largeOutput }))
      }
    }))
    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'answer-large',
          type: 'agentMessage',
          text: '',
          phase: 'final_answer'
        }
      })
    )
    codexApi.emitOutputLine(
      threadKey(parent.ts),
      JSON.stringify({
        type: 'item.agentMessage.delta',
        itemId: 'answer-large',
        delta: 'LARGE_TASK_FINAL_VISIBLE'
      })
    )
    codexApi.emitSessionEvent(threadKey(parent.ts), 'session.execution_completed', {
      execution_id: 'exe-large-task-output',
      status: 'completed'
    })

    await mention.finished()
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const taskChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'task_update')
    expect(taskChunks).not.toHaveLength(0)
    const taskIds = [...Array.from({ length: 6 }, (_, index) => `cmd-large-${index}`), 'edited-files']
    expect([...new Set(taskChunks.map(chunk => stringField(chunk.id)))].sort()).toEqual(taskIds)
    for (const id of taskIds) {
      expect(taskChunks.filter(chunk => chunk.id === id).at(-1)?.status).toBe('complete')
    }
    expect(taskChunks.every(chunk => stringField(chunk.output) === '')).toBe(true)
    expect(taskChunks.every(chunk => !chunkText(chunk).includes('large-context-line'))).toBe(true)
    expect(taskChunks.some(chunk => chunkText(chunk).includes('slack thread --json --page 0'))).toBe(
      true
    )
    for (const chunk of taskChunks) {
      const details = stringField(chunk.details)
      expect(details.length).toBeLessThanOrEqual(256)
      expect(Buffer.from(details).toString('utf8')).toBe(details)
    }
    expect(taskChunks.some(chunk => stringField(chunk.details).includes('[truncated'))).toBe(true)
    const markdownChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'markdown_text')
    expect(markdownChunks).toEqual([
      {
        type: 'markdown_text',
        text: 'LARGE_TASK_FINAL_VISIBLE'
      }
    ])
  })

  it('honors plain-text-only requests without Slack plan blocks', async () => {
    const parent = await postUserMessage('Context before a plain text request.')
    const mention = await sendMessage(
      `<@${BOT_USER_ID}> Answer from context only. Plain text only, no interactive blocks or dashboards. Include id plain-text-regression.`,
      { threadTs: parent.ts }
    )

    expect(mention.response.status).toBe(200)
    await mention.finished()

    expect(slackApi.calls.some(call => call.method === 'chat.startStream')).toBe(false)
    expect(slackApi.calls.some(call => call.method === 'chat.appendStream')).toBe(false)
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(false)

    const text = await threadText(parent.ts)
    expect(text).toContain('Executed request 1.')
    expect(text).not.toContain('Implementation plan')
    expect(text).not.toContain('Command execution')
    expect(text).not.toContain('pnpm test')
  })

  it('shows assistant status while waiting for slow session execute', async () => {
    const logs: CapturedLog[] = []
    bot = createTestBot({ logger: captureLogger(logs) })
    codexApi.autoRespond = false
    const releaseExecute = codexApi.holdNextExecute()

    const parent = await postUserMessage('Context before the slow run.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> start visibly`, parent.ts)
    const waits: Promise<unknown>[] = []
    let responseSettled = false
    const responsePromise = Promise.resolve(
      bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: 'Ev-slackbotv2-slow-execute',
          retry_num: '1',
          retry_reason: 'http_timeout',
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            thread_ts: parent.ts,
            text: `<@${BOT_USER_ID}> start visibly`
          }
        }),
        {},
        waitUntilContext(waits)
      )
    ).then((response: Response) => {
      responseSettled = true
      return response
    })

    await waitFor(() => codexApi.executes.length === 1)
    await sleep(50)
    expect(responseSettled).toBe(false)
    expect(
      slackApi.calls
        .filter(call => call.method === 'assistant.threads.setStatus')
        .map(call => stringField(call.body.status))
    ).toEqual(['Thinking...'])
    expect(slackApi.calls.some(call => call.method === 'chat.startStream')).toBe(false)
    expect(codexApi.eventRequests).toHaveLength(0)
    await waitFor(() => hasLog(logs, 'slackbotv2_webhook_handoff_wait_started'))
    expect(logData(logs, 'slackbotv2_handoff_started')).toEqual(
      expect.objectContaining({
        assistant_status_requested: true,
        message_id: mention.ts,
        mode: 'execute',
        slack_user_id: USER_ID,
        thread_id: threadKey(parent.ts),
        trigger: 'new_mention'
      })
    )
    expect(logData(logs, 'slackbotv2_forward_started')).toEqual(
      expect.objectContaining({
        message_id: mention.ts,
        mode: 'execute',
        slack_user_id: USER_ID,
        thread_id: threadKey(parent.ts)
      })
    )
    expect(logData(logs, 'slackbotv2_assistant_status_started')).toEqual(
      expect.objectContaining({
        message_id: mention.ts,
        operation: 'set',
        slack_user_id: USER_ID,
        thread_id: threadKey(parent.ts)
      })
    )
    expect(logData(logs, 'slackbotv2_assistant_status_complete')).toEqual(
      expect.objectContaining({
        operation: 'set',
        visible: true
      })
    )
    expect(logData(logs, 'slackbotv2_handoff_sync_starting')).toEqual(
      expect.objectContaining({
        initial_assistant_status_visible: expect.any(Boolean),
        trigger: 'new_mention'
      })
    )
    expect(logData(logs, 'slackbotv2_webhook_handoff_wait_started')).toEqual(
      expect.objectContaining({
        slack_channel: CHANNEL_ID,
        slack_event_id: 'Ev-slackbotv2-slow-execute',
        slack_event_type: 'app_mention',
        slack_message_ts: mention.ts,
        slack_retry_num: '1',
        slack_retry_reason: 'http_timeout',
        slack_thread_ts: parent.ts,
        task_count: expect.any(Number)
      })
    )

    releaseExecute()
    const response = await responsePromise
    expect(response.status).toBe(200)
    await waitFor(() => hasLog(logs, 'slackbotv2_webhook_handoff_wait_complete'))
    expect(logData(logs, 'slackbotv2_webhook_handoff_wait_complete')).toEqual(
      expect.objectContaining({
        phase_ms: expect.any(Number),
        slack_event_id: 'Ev-slackbotv2-slow-execute'
      })
    )
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)
    codexApi.closeStreams()
    await Promise.all(waits)
    expect(
      slackApi.calls
        .filter(call => call.method === 'assistant.threads.setStatus')
        .map(call => stringField(call.body.status))
    ).toEqual(expect.arrayContaining(['Thinking...', '']))
  })

  it('does not wait for hung assistant status before creating Slack sessions', async () => {
    const logs: CapturedLog[] = []
    bot = createTestBot({ logger: captureLogger(logs), slackApiTimeoutMs: 25 })
    const releaseStatus = slackApi.holdAssistantStatus()
    const waits: Promise<unknown>[] = []

    try {
      const parent = await postUserMessage('Context before hung status.')
      const mention = await postUserMessage(`<@${BOT_USER_ID}> keep going`, parent.ts)
      const responsePromise = bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: 'Ev-slackbotv2-hung-status',
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            thread_ts: parent.ts,
            text: `<@${BOT_USER_ID}> keep going`
          }
        }),
        {},
        waitUntilContext(waits)
      )

      await waitFor(() => codexApi.creates.length === 1 && codexApi.executes.length === 1)
      const response = await responsePromise
      expect(response.status).toBe(200)
      expect(codexApi.creates[0]?.threadKey).toBe(threadKey(parent.ts))
      expect(codexApi.executes[0]?.threadKey).toBe(threadKey(parent.ts))
      expect(logData(logs, 'slackbotv2_handoff_sync_starting')).toEqual(
        expect.objectContaining({
          initial_assistant_status_deferred: true,
          initial_assistant_status_visible: false,
          trigger: 'new_mention'
        })
      )
      await waitFor(() => hasLog(logs, 'slackbotv2_assistant_status_failed'))
      expect(logData(logs, 'slackbotv2_assistant_status_failed')).toEqual(
        expect.objectContaining({
          error: 'set assistant status timed out after 25ms',
          operation: 'set'
        })
      )
    } finally {
      releaseStatus()
    }
    await Promise.all(waits)
  })

  it('shows visible task progress by default when activity summary status is disabled', async () => {
    bot = createProductionDefaultTestBot()
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before default progress.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> summarize progress`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const key = threadKey(parent.ts)
    const summary = "I'm checking the event stream so I can explain the current state."
    codexApi.emitSessionEvent(key, 'session.activity_summary', {
      execution_id: 'exe-default-visible-progress',
      summary
    })
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-default-progress',
          type: 'commandExecution',
          command: 'rg activity summary',
          status: 'inProgress'
        }
      })
    )
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'turn.done',
        result: 'Default progress done.'
      })
    )

    await mention.finished()
    const statusCalls = slackApi.calls.filter(call => call.method === 'assistant.threads.setStatus')
    expect(statusCalls.map(call => stringField(call.body.status))).toEqual(['Thinking...', ''])
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    expect(transcripts[0]!.start.body.task_display_mode).toBe('plan')
    expect(transcripts[0]!.chunks.some(chunk => chunk.type === 'task_update')).toBe(true)
    const text = await threadText(parent.ts)
    expect(text).toContain('Command execution')
    expect(text).toContain('Default progress done.')
    expect(text).not.toContain(summary)
  })

  it('uses session activity summaries as assistant status instead of visible text', async () => {
    bot = createProductionDefaultTestBot({ activitySummaryStatusEnabled: true })
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before status update.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> summarize activity`, {
      threadTs: parent.ts
    })

    expect(mention.response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const key = threadKey(parent.ts)
    const summary =
      "I'm checking the benchmark page and related logs so I can explain the chart shape."
    const clippedSummary = `${summary.slice(0, 47).trimEnd()}...`
    codexApi.emitSessionEvent(key, 'session.activity_summary', {
      execution_id: 'exe-activity-summary-status',
      summary
    })
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'item.started',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'rg activity summary',
          status: 'inProgress'
        }
      })
    )
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: 'turn.done',
        result: 'Done with status.'
      })
    )

    await mention.finished()
    const statusCalls = slackApi.calls.filter(call => call.method === 'assistant.threads.setStatus')
    expect(statusCalls.map(call => stringField(call.body.status))).toEqual([
      'Thinking...',
      clippedSummary,
      ''
    ])
    expect(Array.from(clippedSummary)).toHaveLength(50)
    expect(statusCalls[1]?.body).toEqual(
      expect.objectContaining({
        channel_id: CHANNEL_ID,
        thread_ts: parent.ts,
        loading_messages: [clippedSummary],
        status: clippedSummary
      })
    )
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    expect(transcripts[0]!.start.body.task_display_mode).toBeUndefined()
    expect(transcripts[0]!.chunks.every(chunk => chunk.type === 'markdown_text')).toBe(true)
    const text = await threadText(parent.ts)
    expect(text).toContain('Done with status.')
    expect(text).not.toContain(summary)
    expect(text).not.toContain('Command execution')
    expect(text).not.toContain('Thinking')
  })

  it('skips stale render obligations from Chat SDK state on startup', async () => {
    const logs: CapturedLog[] = []
    const sharedState = createMemoryState()
    await sharedState.connect()

    const parent = await postUserMessage('Context before stale recovery.')
    const mentionText = `<@${BOT_USER_ID}> this answer is too old to recover`
    const mention = await postUserMessage(mentionText, parent.ts)
    const key = threadKey(parent.ts)
    const message = {
      ...apiMessageFromSlackEvent({
        isMention: true,
        text: mentionText,
        threadId: key,
        ts: mention.ts
      }),
      timestamp: new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString()
    }
    await sharedState.set(`thread-state:${key}`, {
      activeExecution: true,
      executedMessageIds: [mention.ts],
      forwardedMessageIds: [mention.ts],
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exe-stale-recovery',
        message
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', key)
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Stale recovered request.'))

    bot = createTestBot({
      logger: captureLogger(logs),
      renderRecoveryMaxObligationAgeMs: 60 * 60 * 1000,
      state: sharedState
    })

    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return threadState?.renderObligation === null
    }, 2000)

    expect(codexApi.eventRequests).toHaveLength(0)
    expect(slackApi.calls.some(call => call.method === 'chat.startStream')).toBe(false)
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(false)
    const staleState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
    expect(staleState).toEqual(
      expect.objectContaining({ activeExecution: false, renderObligation: null })
    )
    expect(logData(logs, 'slackbotv2_render_recovery_stale_obligation_skipped')).toEqual(
      expect.objectContaining({
        execution_id: 'exe-stale-recovery',
        max_obligation_age_ms: 60 * 60 * 1000,
        message_id: mention.ts,
        thread_id: key
      })
    )
  })

  it('does not let one hung recovery block the obligations queued behind it', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()

    // Thread A's execution has no events at all (a zombie: its SSE opens and
    // never yields a chunk), so its recovery hangs until the per-thread
    // deadline. Thread B is fully renderable and indexed behind A.
    const hungKey = threadKey('1781100000.000001')
    const hungMessage = apiMessageFromSlackEvent({
      isMention: true,
      text: `<@${BOT_USER_ID}> hung recovery`,
      threadId: hungKey,
      ts: '1781100000.000002'
    })
    await sharedState.set(`thread-state:${hungKey}`, {
      activeExecution: true,
      executedMessageIds: [hungMessage.id],
      forwardedMessageIds: [hungMessage.id],
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exe-hung-recovery',
        message: hungMessage
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', hungKey)

    const parent = await postUserMessage('Context before queued recovery.')
    const mentionText = `<@${BOT_USER_ID}> recover behind a zombie`
    const mention = await postUserMessage(mentionText, parent.ts)
    const key = threadKey(parent.ts)
    const message = apiMessageFromSlackEvent({
      isMention: true,
      text: mentionText,
      threadId: key,
      ts: mention.ts
    })
    await sharedState.set(`thread-state:${key}`, {
      activeExecution: true,
      executedMessageIds: [mention.ts],
      forwardedMessageIds: [mention.ts],
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exe-behind-zombie',
        message
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', key)
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Recovered behind the zombie.'))

    bot = createTestBot({ state: sharedState, renderRecoveryThreadTimeoutMs: 200 })

    await waitFor(async () => {
      const recovered = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return recovered?.renderObligation === null
    }, 5000)
    expect(await threadText(parent.ts)).toContain('Recovered behind the zombie.')
    const recoveredState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
    expect(recoveredState).toEqual(
      expect.objectContaining({ activeExecution: false, renderObligation: null })
    )
    // The hung thread stays pending (deferred), not failed or cleared.
    const hungState = await sharedState.get<Record<string, unknown>>(`thread-state:${hungKey}`)
    expect(hungState).toEqual(
      expect.objectContaining({
        renderObligation: expect.objectContaining({ executionId: 'exe-hung-recovery' })
      })
    )
  })

  it('does not duplicate the live render while the recovery sweep is cycling', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()

    // A zombie obligation (its event stream never yields) keeps the recovery
    // sweep loop cycling with short claim timeouts, so sweep passes land
    // while the live render below is still streaming. Without the live
    // render holding the per-thread lease, a pass claims the just-indexed
    // obligation and posts the same answer twice.
    const zombieKey = threadKey('1781200000.000001')
    const zombieMessage = apiMessageFromSlackEvent({
      isMention: true,
      text: `<@${BOT_USER_ID}> zombie`,
      threadId: zombieKey,
      ts: '1781200000.000002'
    })
    await sharedState.set(`thread-state:${zombieKey}`, {
      activeExecution: true,
      executedMessageIds: [zombieMessage.id],
      forwardedMessageIds: [zombieMessage.id],
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exe-sweep-zombie',
        message: zombieMessage
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', zombieKey)

    codexApi.autoRespond = false
    bot = createTestBot({ state: sharedState, renderRecoveryThreadTimeoutMs: 100 })

    const parent = await postUserMessage('Context before sweep race.')
    const mentionText = `<@${BOT_USER_ID}> race the sweep`
    const mention = await postUserMessage(mentionText, parent.ts)
    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      thread_ts: parent.ts,
      text: mentionText
    })
    expect(delivery.response.status).toBe(200)

    const key = threadKey(parent.ts)
    await waitFor(() => codexApi.executes.length === 1, 2000)
    const outputLines = sampleCodexOutputLines('Single answer despite the sweep.')
    // Everything except turn/completed: the live render stays in-flight...
    codexApi.emitOutputLines(key, outputLines.slice(0, -1))
    // ...long enough for several sweep passes to scan the live obligation.
    await sleep(1200)
    codexApi.emitOutputLines(key, outputLines.slice(-1))
    await delivery.finished()
    await waitFor(() => slackApi.calls.some(call => call.method === 'chat.stopStream'), 3000)
    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return threadState?.renderObligation === null
    }, 3000)

    // Exactly one renderer consumed the execution and exactly one Slack
    // stream was started for the live thread.
    expect(codexApi.eventRequests.filter(request => request.threadKey === key)).toHaveLength(1)
    const startsForThread = slackApi.calls.filter(
      call => call.method === 'chat.startStream' && call.body.thread_ts === parent.ts
    )
    expect(startsForThread).toHaveLength(1)
    expect(await threadText(parent.ts)).toContain('Single answer despite the sweep.')
  })

  it('abandons an obligation after repeated non-retryable recovery failures', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()

    // A corrupt thread id without a thread ts: the Slack adapter rejects it
    // on every recovery attempt, mirroring the production obligation that
    // poisoned the scan forever.
    const corruptKey = `slack:${CHANNEL_ID}:`
    const corruptMessage = apiMessageFromSlackEvent({
      isMention: true,
      text: `<@${BOT_USER_ID}> recover the corrupt thread`,
      threadId: corruptKey,
      ts: '1781100001.000001'
    })
    await sharedState.set(`thread-state:${corruptKey}`, {
      activeExecution: true,
      executedMessageIds: [corruptMessage.id],
      forwardedMessageIds: [corruptMessage.id],
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exe-corrupt-thread',
        message: corruptMessage
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', corruptKey)
    codexApi.emitOutputLines(corruptKey, sampleCodexOutputLines('Unreachable answer.'))

    bot = createTestBot({ state: sharedState })

    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(
        `thread-state:${corruptKey}`
      )
      return threadState?.renderObligation === null
    }, 10_000)
    const abandonedState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${corruptKey}`
    )
    expect(abandonedState).toEqual(
      expect.objectContaining({ activeExecution: false, renderObligation: null })
    )
    // Five failing passes with backoff take several seconds.
  }, 15_000)

  it('retries retryable event stream open failures after execute', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false
    codexApi.failNextEvents = true

    const parent = await postUserMessage('Context before stream retry.')
    const mentionText = `<@${BOT_USER_ID}> recover after stream open failure`
    const mention = await postUserMessage(mentionText, parent.ts)
    const key = threadKey(parent.ts)

    const delivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: TEAM_ID,
      ts: mention.ts,
      thread_ts: parent.ts,
      text: mentionText
    })
    expect(delivery.response.status).toBe(200)

    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(false)

    const deferredThreadState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${key}`
    )
    expect(deferredThreadState).toEqual(
      expect.objectContaining({
        activeExecution: true,
        renderObligation: expect.any(Object)
      })
    )

    codexApi.emitOutputLines(key, sampleCodexOutputLines('Recovered after stream retry.'))
    await waitFor(() => codexApi.eventRequests.length >= 2, 3000)
    await waitFor(() => slackApi.calls.some(call => call.method === 'chat.stopStream'), 3000)
    await delivery.finished()

    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.eventRequests).toEqual([
      { afterEventId: 0, executionId: 'exe-1', threadKey: key },
      { afterEventId: 0, executionId: 'exe-1', threadKey: key }
    ])
    expect(await threadText(parent.ts)).toContain('Recovered after stream retry.')
    // Recovery clears the obligation after the Slack stream stops; wait for
    // the state write instead of racing it.
    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return threadState?.renderObligation === null
    }, 2000)
    const recoveredThreadState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${key}`
    )
    expect(recoveredThreadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        lastEventId: expect.any(Number),
        renderObligation: null
      })
    )
    expect(Number(recoveredThreadState?.lastEventId)).toBeGreaterThan(0)
  })

  it('locally retries a retryable execute failure without duplicate append', async () => {
    bot = createTestBot({ handoffRetryDelaysMs: [50] })
    codexApi.failNextExecute = true

    const parent = await postUserMessage('History that must not be lost.')
    const failedMention = await postUserMessage(`<@${BOT_USER_ID}> first try`, parent.ts)
    const retryableEvent = signedSlackEvent({
      event_id: 'Ev-slackbotv2-retryable-mention',
      event: {
        type: 'app_mention',
        user: USER_ID,
        channel: CHANNEL_ID,
        team: TEAM_ID,
        ts: failedMention.ts,
        thread_ts: parent.ts,
        text: `<@${BOT_USER_ID}> first try`
      }
    })
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(waits)
    )
    // The retryable failure is retried in-process; Slack is acknowledged so
    // its own redelivery (which would be deduped anyway) is never needed.
    expect(response.status).toBe(200)
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.eventRequests).toHaveLength(0)

    await waitFor(() => codexApi.executes.length === 2, 3000)
    await waitFor(async () => (await threadText(parent.ts)).includes('Executed request 1.'), 3000)
    await Promise.all(waits)

    expect(codexApi.appends).toHaveLength(1)
    const retryContextTexts = sessionMessageTexts(codexApi.appends[0]?.body.messages ?? [])
    expect(retryContextTexts).toContain('History that must not be lost.')
    expect(retryContextTexts.some(text => text.includes('first try'))).toBe(true)
    expect(codexApi.eventRequests).toHaveLength(1)

    // A late Slack redelivery of the same event stays deduped and adds no work.
    const redeliveryWaits: Promise<unknown>[] = []
    const redeliveryResponse = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(redeliveryWaits)
    )
    expect(redeliveryResponse.status).toBe(200)
    await Promise.all(redeliveryWaits)
    expect(codexApi.executes).toHaveLength(2)
    expect(codexApi.appends).toHaveLength(1)
  })

  it('keeps a steering reaction when an append retry starts an execution', async () => {
    let failNextAppend = false
    bot = createTestBot({
      fetch: async (input, init) => {
        if (failNextAppend && String(input).endsWith('/messages')) {
          failNextAppend = false
          return new Response('unavailable', {
            status: 503,
            statusText: 'Service Unavailable'
          })
        }
        return globalThis.fetch(input, init)
      },
      handoffRetryDelaysMs: [300],
      steeringReactionEnabled: true
    })
    codexApi.autoRespond = false

    const parent = await postUserMessage('History before an upgraded steering retry.')
    const firstMention = await sendMessage(`<@${BOT_USER_ID}> start running`, {
      threadTs: parent.ts
    })
    expect(firstMention.response.status).toBe(200)
    await waitFor(() => codexApi.streamCount === 1)

    failNextAppend = true
    const steeringMention = await sendMessage(`<@${BOT_USER_ID}> retry me as the next turn`, {
      threadTs: parent.ts
    })
    expect(steeringMention.response.status).toBe(200)
    await waitFor(() => slackApi.calls.some(call => call.method === 'reactions.add'))
    expect(slackApi.calls.some(call => call.method === 'reactions.remove')).toBe(false)

    const key = threadKey(parent.ts)
    codexApi.emitOutputLines(key, sampleCodexOutputLines('First execution complete.'))
    await firstMention.finished()

    await waitFor(() => codexApi.executes.length === 2, 3000)
    expect(slackApi.calls.some(call => call.method === 'reactions.remove')).toBe(false)

    await waitFor(() => codexApi.eventRequests.length === 2, 3000)
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Upgraded retry complete.'))
    await steeringMention.finished()
    await waitFor(() => slackApi.calls.some(call => call.method === 'reactions.remove'), 3000)
    expect(
      slackApi.calls
        .filter(call => call.method === 'reactions.add' || call.method === 'reactions.remove')
        .map(call => ({
          method: call.method,
          timestamp: stringField(call.body.timestamp)
        }))
    ).toEqual([
      { method: 'reactions.add', timestamp: steeringMention.ts },
      { method: 'reactions.remove', timestamp: steeringMention.ts }
    ])
    expect(await threadText(parent.ts)).toContain('Upgraded retry complete.')
  })

  it('reuses an accepted execution when the local retry follows a lost execute response', async () => {
    let overrideStrategyCalls = 0
    bot = createTestBot({
      handoffRetryDelaysMs: [50],
      messageOverridesStrategy: async () => {
        overrideStrategyCalls += 1
        return {
          overrides: {
            harnessType: overrideStrategyCalls === 1 ? 'claudecode' : 'codex',
            model: overrideStrategyCalls === 1 ? 'claude-opus-4-8' : 'gpt-5.6-sol'
          }
        }
      }
    })
    codexApi.failNextExecuteAfterAccept = true

    const parent = await postUserMessage('History before response loss.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> first try accepted`, parent.ts)
    const retryableEvent = signedSlackEvent({
      event_id: 'Ev-slackbotv2-execute-response-lost',
      event: {
        type: 'app_mention',
        user: USER_ID,
        channel: CHANNEL_ID,
        team: TEAM_ID,
        ts: mention.ts,
        thread_ts: parent.ts,
        text: `<@${BOT_USER_ID}> first try accepted`
      }
    })
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(waits)
    )
    expect(response.status).toBe(200)
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.eventRequests).toHaveLength(0)

    await waitFor(() => codexApi.executes.length === 2, 3000)
    await waitFor(async () => (await threadText(parent.ts)).includes('Executed request 1.'), 3000)
    await Promise.all(waits)

    expect(codexApi.executes.map(execute => execute.body.idempotency_key)).toEqual([
      mention.ts,
      mention.ts
    ])
    expect(overrideStrategyCalls).toBe(1)
    expect(
      codexApi.executes.map(execute =>
        JSON.parse(execute.body.input_lines.at(-1) ?? '{}') as Record<string, unknown>
      )
    ).toEqual([
      expect.objectContaining({ model: 'claude-opus-4-8' }),
      expect.objectContaining({ model: 'claude-opus-4-8' })
    ])
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.eventRequests).toHaveLength(1)
    expect(await threadText(parent.ts)).not.toContain('Executed request 2.')
  })

  it('conflates a pending execute retry into an execution started meanwhile', async () => {
    bot = createTestBot({ handoffRetryDelaysMs: [300] })
    codexApi.failNextExecute = true

    const parent = await postUserMessage('History before conflation.')
    const firstMention = await sendMessage(`<@${BOT_USER_ID}> first conflated mention`, {
      threadTs: parent.ts
    })
    expect(firstMention.response.status).toBe(200)
    expect(codexApi.executes).toHaveLength(1)

    // A second mention lands while the first message's retry is still pending
    // and starts the thread's execution. Keep it running (no auto response)
    // across the retry window.
    codexApi.autoRespond = false
    const secondMention = await sendMessage(`<@${BOT_USER_ID}> second conflated mention`, {
      threadTs: parent.ts
    })
    expect(secondMention.response.status).toBe(200)
    expect(codexApi.executes).toHaveLength(2)

    // The first message's retry fires into the active execution and must not
    // start a third execution; its text is already in the session, so the
    // running execution sees it.
    await sleep(500)
    expect(codexApi.executes).toHaveLength(2)
    const appendedTexts = codexApi.appends.flatMap(append =>
      sessionMessageTexts(append.body.messages ?? [])
    )
    expect(appendedTexts.some(text => text.includes('first conflated mention'))).toBe(true)
    expect(appendedTexts.some(text => text.includes('second conflated mention'))).toBe(true)

    codexApi.emitOutputLines(threadKey(parent.ts), sampleCodexOutputLines('Conflated answer.'))
    await Promise.all([firstMention.finished(), secondMention.finished()])
    expect(await threadText(parent.ts)).toContain('Conflated answer.')
    expect(await threadText(parent.ts)).not.toContain(BROKEN_STREAM_TEXT)
  })

  it('renders a visible error once local retries are exhausted', async () => {
    bot = createTestBot({ handoffRetryDelaysMs: [200] })
    codexApi.failNextExecute = true

    const parent = await postUserMessage('History before exhaustion.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> exhaust retries`, { threadTs: parent.ts })
    expect(mention.response.status).toBe(200)
    expect(codexApi.executes).toHaveLength(1)

    // Fail the scheduled retry too so the budget of one retry is exhausted.
    codexApi.failNextExecute = true
    await waitFor(() => codexApi.executes.length === 2, 3000)
    await waitFor(async () => (await threadText(parent.ts)).includes('Execution failed'), 3000)
    await mention.finished()

    expect(codexApi.eventRequests).toHaveLength(0)
    const threadState = await bot.chat
      .thread(threadKey(parent.ts))
      .state
    expect(threadState).toEqual(expect.objectContaining({ activeExecution: false }))
  })

  it('silently ignores a session principal admission denial after clearing deferred status', async () => {
    bot = createTestBot({ assistantStatus: 'Admission pending...' })
    const releaseStatus = slackApi.holdAssistantStatus()
    codexApi.queueCreateResponse(
      {
        ok: false,
        error: 'session principal slack-channel-t123-c123 is not preapproved',
        code: 'session_principal_not_preapproved'
      },
      403
    )

    const parent = await postUserMessage('History before admission denial.')
    const mention = await sendMessage(`<@${BOT_USER_ID}> denied`, { threadTs: parent.ts })

    try {
      expect(mention.response.status).toBe(200)
      await waitFor(() => codexApi.creates.length === 1)
      const initialStatusIndex = slackApi.calls.findIndex(
        call =>
          call.method === 'assistant.threads.setStatus'
          && stringField(call.body.status) === 'Admission pending...'
      )
      expect(initialStatusIndex).toBeGreaterThanOrEqual(0)
      expect(
        slackApi.calls
          .slice(initialStatusIndex)
          .filter(call => call.method === 'assistant.threads.setStatus')
          .map(call => stringField(call.body.status))
      ).toEqual(['Admission pending...'])

      releaseStatus()
      await mention.finished()
      expect(codexApi.creates).toHaveLength(1)
      expect(codexApi.appends).toHaveLength(0)
      expect(codexApi.executes).toHaveLength(0)
      expect(
        slackApi.calls
          .slice(initialStatusIndex)
          .filter(call => call.method === 'assistant.threads.setStatus')
          .map(call => stringField(call.body.status))
      ).toEqual(['Admission pending...', ''])
      expect(await threadText(parent.ts)).not.toContain('Execution failed')
    } finally {
      releaseStatus()
    }
  })

  it('enforces external org and trigger-bot member allowlists', async () => {
    const externalMention = await postUserMessage(`<@${BOT_USER_ID}> from external org`)
    const externalDelivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: 'TEXTERNAL',
      user_team: 'TEXTERNAL',
      ts: externalMention.ts,
      text: `<@${BOT_USER_ID}> from external org`
    })
    expect(externalDelivery.response.status).toBe(200)
    await externalDelivery.finished()
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)

    bot = createTestBot({ allowedExternalTeamIds: ['TEXTERNAL'] })
    const allowedExternalMention = await postUserMessage(`<@${BOT_USER_ID}> allowed external org`)
    const allowedExternalDelivery = await deliverEvent({
      type: 'app_mention',
      user: USER_ID,
      channel: CHANNEL_ID,
      team: 'TEXTERNAL',
      user_team: 'TEXTERNAL',
      ts: allowedExternalMention.ts,
      text: `<@${BOT_USER_ID}> allowed external org`
    })
    expect(allowedExternalDelivery.response.status).toBe(200)
    await allowedExternalDelivery.finished()
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)

    bot = createTestBot()
    codexApi.reset()
    const botMention = await postUserMessage(`<@${BOT_USER_ID}> from another bot`)
    const botDelivery = await deliverEvent({
      type: 'app_mention',
      app_id: 'AOTHERBOT',
      bot_id: 'BOTHERBOT',
      bot_profile: {
        app_id: 'AOTHERBOT',
        id: 'BOTHERBOT',
        user_id: 'UOTHERBOT'
      },
      channel: CHANNEL_ID,
      team: TEAM_ID,
      text: `<@${BOT_USER_ID}> from another bot`,
      ts: botMention.ts,
      user: 'UOTHERBOT',
      username: 'otherbot'
    })
    expect(botDelivery.response.status).toBe(200)
    await botDelivery.finished()
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)

    bot = createTestBot({ triggerBotAllowlist: ['UOTHERBOT'] })
    const allowedBotMention = await postUserMessage(`<@${BOT_USER_ID}> from allowed bot`)
    const allowedBotDelivery = await deliverEvent({
      type: 'app_mention',
      app_id: 'AOTHERBOT',
      bot_id: 'BOTHERBOT',
      bot_profile: {
        app_id: 'AOTHERBOT',
        id: 'BOTHERBOT',
        user_id: 'UOTHERBOT'
      },
      channel: CHANNEL_ID,
      team: TEAM_ID,
      text: `<@${BOT_USER_ID}> from allowed bot`,
      ts: allowedBotMention.ts,
      user: 'UOTHERBOT',
      username: 'otherbot'
    })
    expect(allowedBotDelivery.response.status).toBe(200)
    await allowedBotDelivery.finished()
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)

    bot = createTestBot({ triggerBotAllowlist: ['UOTHERBOT'] })
    codexApi.reset()
    const labeledBotMention = `<@${BOT_USER_ID}|centaur> from allowed bot message`
    const allowedBotChannelMessage = await postUserMessage(labeledBotMention)
    slackApi.reset()
    const allowedBotChannelDelivery = await deliverEvent({
      type: 'message',
      app_id: 'AOTHERBOT',
      bot_id: 'BOTHERBOT',
      bot_profile: {
        app_id: 'AOTHERBOT',
        id: 'BOTHERBOT',
        user_id: 'UOTHERBOT'
      },
      channel: CHANNEL_ID,
      subtype: 'bot_message',
      team: TEAM_ID,
      text: labeledBotMention,
      ts: allowedBotChannelMessage.ts,
      username: 'otherbot'
    })
    expect(allowedBotChannelDelivery.response.status).toBe(200)
    await allowedBotChannelDelivery.finished()
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)
    const allowedBotChannelTranscripts = slackStreamTranscripts(slackApi.calls)
    expect(allowedBotChannelTranscripts).toHaveLength(1)
    expect(allowedBotChannelTranscripts[0]!.start.body).toEqual(
      expect.objectContaining({
        recipient_team_id: TEAM_ID,
        recipient_user_id: 'UOTHERBOT'
      })
    )

    bot = createTestBot({ triggerBotAllowlist: ['UOTHERBOT'] })
    codexApi.reset()
    slackApi.reset()
    const richBotMessage = await postUserMessage('attachment-only event placeholder')
    const richBotDelivery = await deliverEvent({
      type: 'message',
      app_id: 'AOTHERBOT',
      attachments: [
        {
          pretext: `<@${BOT_USER_ID}> investigate`,
          title: ':red_circle: Validator stalled',
          text: '*Cluster:* stg-na\n*Tenant:* luganodes'
        }
      ],
      bot_id: 'BOTHERBOT',
      bot_profile: {
        app_id: 'AOTHERBOT',
        id: 'BOTHERBOT',
        user_id: 'UOTHERBOT'
      },
      channel: CHANNEL_ID,
      subtype: 'bot_message',
      team: TEAM_ID,
      text: '',
      ts: richBotMessage.ts,
      username: 'otherbot'
    })
    expect(richBotDelivery.response.status).toBe(200)
    await richBotDelivery.finished()
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)
    expect(sessionMessageTexts(codexApi.appends[0]!.body.messages).join('\n')).toContain(
      'Validator stalled'
    )

    bot = createTestBot()
    codexApi.reset()
    const deniedRichBotMessage = await postUserMessage('attachment-only event placeholder')
    const deniedRichBotDelivery = await deliverEvent({
      type: 'message',
      attachments: [{ pretext: `<@${BOT_USER_ID}> investigate` }],
      bot_id: 'BOTHERBOT',
      channel: CHANNEL_ID,
      subtype: 'bot_message',
      team: TEAM_ID,
      text: '',
      ts: deniedRichBotMessage.ts,
      username: 'otherbot'
    })
    expect(deniedRichBotDelivery.response.status).toBe(200)
    await deniedRichBotDelivery.finished()
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)
  })

  it('streams bot-authored trigger replies to a resolvable member-id recipient', async () => {
    // Regression for bot triggers whose raw event carries only the bot's
    // `B...` id: that id fails Slack's `^[UW][A-Z0-9]{2,}$` recipient pattern,
    // so the render path must re-resolve the bot's `U...` id (the cached
    // allowlist identity) before starting the structured stream.
    slackApi.setBotInfo('BOTHERBOT', { app_id: 'AOTHERBOT', id: 'BOTHERBOT', user_id: 'UOTHERBOT' })
    const logs: CapturedLog[] = []
    bot = createTestBot({ logger: captureLogger(logs), triggerBotAllowlist: ['UOTHERBOT'] })
    const botTrigger = await postUserMessage('bot-authored trigger placeholder')
    const delivery = await deliverEvent({
      type: 'message',
      bot_id: 'BOTHERBOT',
      channel: CHANNEL_ID,
      subtype: 'bot_message',
      team: TEAM_ID,
      text: `<@${BOT_USER_ID}> run the bot-triggered turn`,
      ts: botTrigger.ts,
      username: 'otherbot'
    })
    expect(delivery.response.status).toBe(200)
    await delivery.finished()
    expect(codexApi.executes).toHaveLength(1)
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    expect(transcripts[0]!.start.body).toEqual(
      expect.objectContaining({
        recipient_team_id: TEAM_ID,
        recipient_user_id: 'UOTHERBOT',
        thread_ts: botTrigger.ts
      })
    )
    // The allowlist gate and the render path share one cached bots.info lookup.
    expect(slackApi.botInfoRequestCount('BOTHERBOT')).toBe(1)
    expect(await threadText(botTrigger.ts)).toContain('Executed request 1.')
    expect(hasLog(logs, 'slackbotv2_render_failed')).toBe(false)
  })
})

function createTestBot(
  overrides: Partial<Parameters<typeof createSlackbotV2>[0]> = {}
): SlackbotV2 {
  return createProductionDefaultTestBot({
    // Most tests in this file pin the structured progress renderer explicitly.
    streamTaskDisplayMode: 'plan',
    ...overrides
  })
}

function createProductionDefaultTestBot(
  overrides: Partial<Parameters<typeof createSlackbotV2>[0]> = {}
): SlackbotV2 {
  const instance = createSlackbotV2({
    apiKey: 'slackbotv2-api-key',
    apiUrl: codexApi.url,
    botToken: BOT_TOKEN,
    botUserId: BOT_USER_ID,
    signingSecret: SIGNING_SECRET,
    slackApiUrl,
    state: createMemoryState(),
    ...overrides
  })
  Object.assign(instance.chat.getAdapter('slack'), {
    createFileTransport: () => (url: URL, signal: AbortSignal, headers: Record<string, string>) => {
      if (url.hostname !== 'files.slack.com') throw new Error('Unexpected fixture download host')
      return new Promise<IncomingMessage>((resolve, reject) => {
        const request = httpRequest(new URL(url.pathname, slackApi.url), { signal, headers }, resolve)
        request.on('error', reject)
        request.end()
      })
    }
  })
  return instance
}

type CapturedLog = {
  data?: unknown
  event: string
  level: 'debug' | 'info' | 'warn' | 'error'
}

function captureLogger(
  logs: CapturedLog[]
): NonNullable<Parameters<typeof createSlackbotV2>[0]['logger']> {
  const logger: NonNullable<Parameters<typeof createSlackbotV2>[0]['logger']> = {
    debug: (event: string, data?: unknown) => logs.push({ data, event, level: 'debug' }),
    info: (event: string, data?: unknown) => logs.push({ data, event, level: 'info' }),
    warn: (event: string, data?: unknown) => logs.push({ data, event, level: 'warn' }),
    error: (event: string, data?: unknown) => logs.push({ data, event, level: 'error' }),
    child: () => logger
  }
  return logger
}

function hasLog(logs: CapturedLog[], event: string): boolean {
  return logs.some(log => log.event === event)
}

function logData(logs: CapturedLog[], event: string): Record<string, unknown> | undefined {
  const data = logs.find(log => log.event === event)?.data
  return isRecord(data) ? data : undefined
}

function sampleCodexNotifications(answer: string): ServerNotification[] {
  return [
    {
      method: 'thread/name/updated',
      params: {
        threadId: 'thread-1',
        threadName: answer.replace('Executed request', 'Codex request').replace('.', '')
      }
    },
    {
      method: 'turn/started',
      params: {
        threadId: 'thread-1',
        turn: {
          id: 'turn-1',
          items: [],
          itemsView: 'full',
          status: 'inProgress',
          error: null,
          startedAt: 1,
          completedAt: null,
          durationMs: null
        }
      }
    },
    {
      method: 'item/started',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        startedAtMs: 2,
        item: {
          type: 'agentMessage',
          id: 'commentary-1',
          text: '',
          phase: 'commentary',
          memoryCitation: null
        }
      }
    },
    {
      method: 'item/started',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        startedAtMs: 4,
        item: {
          type: 'agentMessage',
          id: 'answer-1',
          text: '',
          phase: 'final_answer',
          memoryCitation: null
        }
      }
    },
    {
      method: 'item/agentMessage/delta',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        itemId: 'commentary-1',
        delta: 'Checking the command output'
      }
    },
    {
      method: 'item/reasoning/summaryTextDelta',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        itemId: 'reasoning-1',
        summaryIndex: 0,
        delta: 'Inspecting the event stream'
      }
    },
    {
      method: 'item/completed',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        completedAtMs: 2,
        item: {
          type: 'agentMessage',
          id: 'commentary-1',
          text: 'Checking the command output',
          phase: 'commentary',
          memoryCitation: null
        }
      }
    },
    {
      method: 'turn/plan/updated',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        explanation: 'Implementation plan',
        plan: [
          { step: 'Inspect App Server events', status: 'completed' },
          { step: 'Stream Chat SDK chunks', status: 'inProgress' }
        ]
      }
    },
    {
      method: 'item/started',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        startedAtMs: 2,
        item: {
          type: 'commandExecution',
          id: 'cmd-1',
          command: 'pnpm test',
          cwd: '/repo',
          processId: 'proc-1',
          source: 'agent',
          status: 'inProgress',
          commandActions: [],
          aggregatedOutput: null,
          exitCode: null,
          durationMs: null
        }
      }
    },
    {
      method: 'item/commandExecution/outputDelta',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        itemId: 'cmd-1',
        delta: 'tests passed\n'
      }
    },
    {
      method: 'item/completed',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        completedAtMs: 3,
        item: {
          type: 'commandExecution',
          id: 'cmd-1',
          command: 'pnpm test',
          cwd: '/repo',
          processId: 'proc-1',
          source: 'agent',
          status: 'completed',
          commandActions: [],
          aggregatedOutput: 'tests passed\n',
          exitCode: 0,
          durationMs: 50
        }
      }
    },
    {
      method: 'item/agentMessage/delta',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        itemId: 'answer-1',
        delta: answer
      }
    }
  ] as unknown as ServerNotification[]
}

function sampleCodexOutputLines(answer: string): string[] {
  return [
    ...sampleCodexNotifications(answer).map(notification => JSON.stringify(notification)),
    JSON.stringify({
      method: 'turn/completed',
      params: {
        threadId: 'thread-1',
        turn: {
          id: 'turn-1',
          items: [],
          itemsView: 'full',
          status: 'completed',
          error: null,
          startedAt: 1,
          completedAt: 2,
          durationMs: 1
        }
      }
    })
  ]
}

function sessionMessageTexts(messages: SlackbotV2SessionMessage[]): string[] {
  return messages.flatMap(message =>
    message.parts.flatMap(part => {
      if (isRecord(part) && part.type === 'text' && typeof part.text === 'string') {
        return [part.text]
      }
      return []
    })
  )
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function threadKey(threadTs: string): string {
  return `slack:${CHANNEL_ID}:${threadTs}`
}

function apiMessageFromSlackEvent(input: {
  isMention: boolean
  text: string
  threadId: string
  ts: string
}): SlackbotV2ApiMessage {
  const threadTs = input.threadId.split(':')[2] ?? input.ts
  return {
    attachments: [],
    author: {
      fullName: 'Test User',
      isBot: false,
      isMe: false,
      userId: USER_ID,
      userName: 'tester'
    },
    id: input.ts,
    isMention: input.isMention,
    raw: {
      channel: CHANNEL_ID,
      team: TEAM_ID,
      team_id: TEAM_ID,
      text: input.text,
      thread_ts: threadTs,
      ts: input.ts,
      type: input.isMention ? 'app_mention' : 'message',
      user: USER_ID
    },
    teamId: TEAM_ID,
    text: input.text,
    threadId: input.threadId,
    timestamp: new Date().toISOString()
  }
}

async function postUserMessage(
  text: string,
  threadTs?: string,
  client: WebClient = slack
): Promise<{ ts: string }> {
  const response = await client.chat.postMessage({ channel: CHANNEL_ID, text, thread_ts: threadTs })
  expect(response.ok).toBe(true)
  return { ts: String(response.ts) }
}

async function threadText(threadTs: string): Promise<string> {
  return (await threadTexts(threadTs)).join('\n')
}

async function threadTexts(threadTs: string): Promise<string[]> {
  const response = await slack.conversations.replies({
    channel: CHANNEL_ID,
    ts: threadTs,
    limit: 20
  })
  return (response.messages ?? []).map(message => message.text ?? '')
}

function signedSlackEvent(input: {
  event_id: string
  event: Record<string, unknown>
  retry_num?: string
  retry_reason?: string
}): RequestInit {
  const timestamp = Math.floor(Date.now() / 1000)
  const body = JSON.stringify({
    type: 'event_callback',
    token: 'verification-token',
    team_id: TEAM_ID,
    api_app_id: 'A000000001',
    event_id: input.event_id,
    event_time: timestamp,
    event: input.event
  })
  const signature = createHmac('sha256', SIGNING_SECRET)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')
  const headers: Record<string, string> = {
    'content-type': 'application/json',
    'x-slack-request-timestamp': String(timestamp),
    'x-slack-signature': `v0=${signature}`
  }
  if (input.retry_num) headers['x-slack-retry-num'] = input.retry_num
  if (input.retry_reason) headers['x-slack-retry-reason'] = input.retry_reason
  return {
    method: 'POST',
    headers,
    body
  }
}

function signedSlackInteraction(payload: Record<string, unknown>): RequestInit {
  const timestamp = Math.floor(Date.now() / 1000)
  const body = `payload=${encodeURIComponent(JSON.stringify(payload))}`
  const signature = createHmac('sha256', SIGNING_SECRET)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')
  return {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      'x-slack-request-timestamp': String(timestamp),
      'x-slack-signature': `v0=${signature}`
    },
    body
  }
}

/**
 * A Slack event the bot received: its webhook response, and `finished`, which
 * settles once the work the webhook scheduled has finished.
 */
type Delivery = { response: Response; finished: () => Promise<unknown> }

let eventCount = 0

/** Delivers `event` to the bot's Slack webhook, signed the way Slack signs it. */
async function deliverEvent(
  event: Record<string, unknown>,
  options: { eventId?: string; route?: string; retryNum?: string; retryReason?: string } = {}
): Promise<Delivery> {
  const waits: Promise<unknown>[] = []
  const response = await bot.app.request(
    options.route ?? '/api/webhooks/slack',
    signedSlackEvent({
      event_id: options.eventId ?? `Ev-slackbotv2-${++eventCount}`,
      event,
      retry_num: options.retryNum,
      retry_reason: options.retryReason
    }),
    {},
    waitUntilContext(waits)
  )
  return { response, finished: () => Promise.all(waits) }
}

/**
 * Posts `text` to the test channel as a Slack user (in `threadTs`'s thread when
 * given) and delivers it to the bot the way Slack would: as an app_mention when
 * it mentions the bot, otherwise as a message. `event` adds or replaces fields.
 */
async function sendMessage(
  text: string,
  options: { threadTs?: string; client?: WebClient; event?: Record<string, unknown> } = {}
): Promise<{ ts: string } & Delivery> {
  const client = options.client ?? slack
  const posted = await postUserMessage(text, options.threadTs, client)
  const delivery = await deliverEvent({
    type: text.includes(`<@${BOT_USER_ID}>`) ? 'app_mention' : 'message',
    user: client === slackB ? USER_B_ID : USER_ID,
    channel: CHANNEL_ID,
    team: TEAM_ID,
    ts: posted.ts,
    ...(options.threadTs ? { thread_ts: options.threadTs } : {}),
    text,
    ...options.event
  })
  return { ...posted, ...delivery }
}

function waitUntilContext(waits: Promise<unknown>[]) {
  return {
    waitUntil(promise: Promise<unknown>) {
      waits.push(promise)
    },
    passThroughOnException() {},
    props: {}
  }
}

type MockSessionRequest<T> = {
  body: T
  threadKey: string
}

type MockSessionEventRequest = {
  afterEventId: number
  executionId?: string
  threadKey: string
}

type MockSessionEvent = {
  data: string
  event: string
  executionId?: string
  id: number
  threadKey: string
}

type MockWorkflowEventRequest = {
  event_name: string
  payload: SlackbotV2BlockActionPayload
}

type MockSessionApi = {
  appends: MockSessionRequest<SlackbotV2AppendMessagesRequest>[]
  autoRespond: boolean
  close(): Promise<void>
  closeStreams(): void
  creates: MockSessionRequest<SlackbotV2CreateSessionRequest>[]
  emitOutputLine(threadKey: string, line: string, executionId?: string): void
  emitOutputLines(threadKey: string, lines: string[], executionId?: string): void
  emitSessionEvent(threadKey: string, event: string, data: unknown, executionId?: string): void
  eventRequests: MockSessionEventRequest[]
  executes: MockSessionRequest<SlackbotV2ExecuteSessionRequest>[]
  failNextEvents: boolean
  failNextExecute: boolean
  failNextExecuteAfterAccept: boolean
  holdNextExecute(): () => void
  queueCreateResponse(body: Record<string, unknown>, status?: number): void
  reset(): void
  streamCount: number
  url: string
  workflowEvents: MockWorkflowEventRequest[]
}

async function startMockCodexApi(): Promise<MockSessionApi> {
  const appends: MockSessionRequest<SlackbotV2AppendMessagesRequest>[] = []
  const createResponses: Array<{ body: Record<string, unknown>; status: number }> = []
  const creates: MockSessionRequest<SlackbotV2CreateSessionRequest>[] = []
  const eventRequests: MockSessionEventRequest[] = []
  const events: MockSessionEvent[] = []
  const executes: MockSessionRequest<SlackbotV2ExecuteSessionRequest>[] = []
  const idempotentExecutions = new Map<string, string>()
  const streams = new Set<ServerResponse>()
  const workflowEvents: MockWorkflowEventRequest[] = []
  let autoRespond = true
  let executeHold: Promise<void> | null = null
  let executeHoldRelease: (() => void) | null = null
  let eventId = 0
  let failNextEvents = false
  let failNextExecute = false
  let failNextExecuteAfterAccept = false
  const port = await availablePort(4063)
  const closeStreams = () => {
    for (const stream of streams) stream.end()
    streams.clear()
  }
  const server = createServer((req, res) => {
    void handleMockCodexRequest(req, res, {
      appends,
      createResponses,
      creates,
      events,
      eventRequests,
      executes,
      get autoRespond() {
        return autoRespond
      },
      get executeHold() {
        return executeHold
      },
      get failNextExecute() {
        return failNextExecute
      },
      get failNextExecuteAfterAccept() {
        return failNextExecuteAfterAccept
      },
      get failNextEvents() {
        return failNextEvents
      },
      idempotentExecutions,
      nextEventId() {
        eventId += 1
        return eventId
      },
      port,
      setFailNextEvents(value) {
        failNextEvents = value
      },
      setFailNextExecute(value) {
        failNextExecute = value
      },
      setFailNextExecuteAfterAccept(value) {
        failNextExecuteAfterAccept = value
      },
      streams,
      workflowEvents
    }).catch(error => {
      res.writeHead(500, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ error: String(error) }))
    })
  })
  await listen(server, port)

  const api: MockSessionApi = {
    appends,
    creates,
    eventRequests,
    executes,
    reset() {
      appends.length = 0
      createResponses.length = 0
      creates.length = 0
      eventRequests.length = 0
      events.length = 0
      executes.length = 0
      idempotentExecutions.clear()
      executeHoldRelease?.()
      executeHold = null
      executeHoldRelease = null
      closeStreams()
      autoRespond = true
      eventId = 0
      failNextEvents = false
      failNextExecute = false
      failNextExecuteAfterAccept = false
      workflowEvents.length = 0
    },
    queueCreateResponse(body: Record<string, unknown>, status = 200) {
      createResponses.push({ body, status })
    },
    url: `http://127.0.0.1:${port}`,
    workflowEvents,
    closeStreams,
    get autoRespond() {
      return autoRespond
    },
    set autoRespond(value: boolean) {
      autoRespond = value
    },
    get failNextExecute() {
      return failNextExecute
    },
    set failNextExecute(value: boolean) {
      failNextExecute = value
    },
    get failNextExecuteAfterAccept() {
      return failNextExecuteAfterAccept
    },
    set failNextExecuteAfterAccept(value: boolean) {
      failNextExecuteAfterAccept = value
    },
    get failNextEvents() {
      return failNextEvents
    },
    set failNextEvents(value: boolean) {
      failNextEvents = value
    },
    holdNextExecute() {
      if (executeHoldRelease) throw new Error('execute is already held')
      executeHold = new Promise(resolve => {
        executeHoldRelease = resolve
      })
      return () => {
        const release = executeHoldRelease
        executeHoldRelease = null
        executeHold = null
        release?.()
      }
    },
    get streamCount() {
      return streams.size
    },
    emitOutputLine(threadKey: string, line: string, executionId?: string) {
      emitMockSessionEvent({
        data: line,
        event: 'session.output.line',
        executionId,
        events,
        id: ++eventId,
        streams,
        threadKey
      })
    },
    emitOutputLines(threadKey: string, lines: string[], executionId?: string) {
      for (const line of lines) api.emitOutputLine(threadKey, line, executionId)
    },
    emitSessionEvent(threadKey: string, event: string, data: unknown, executionId?: string) {
      emitMockSessionEvent({
        data: typeof data === 'string' ? data : JSON.stringify(data),
        event,
        executionId,
        events,
        id: ++eventId,
        streams,
        threadKey
      })
    },
    async close() {
      closeStreams()
      await closeServer(server)
    }
  }
  return api
}

async function handleMockCodexRequest(
  req: IncomingMessage,
  res: ServerResponse,
  input: {
    appends: MockSessionRequest<SlackbotV2AppendMessagesRequest>[]
    autoRespond: boolean
    createResponses: Array<{ body: Record<string, unknown>; status: number }>
    creates: MockSessionRequest<SlackbotV2CreateSessionRequest>[]
    events: MockSessionEvent[]
    eventRequests: MockSessionEventRequest[]
    executeHold: Promise<void> | null
    executes: MockSessionRequest<SlackbotV2ExecuteSessionRequest>[]
    failNextExecuteAfterAccept: boolean
    failNextEvents: boolean
    failNextExecute: boolean
    idempotentExecutions: Map<string, string>
    nextEventId(): number
    port: number
    setFailNextEvents(value: boolean): void
    setFailNextExecute(value: boolean): void
    setFailNextExecuteAfterAccept(value: boolean): void
    streams: Set<ServerResponse>
    workflowEvents: MockWorkflowEventRequest[]
  }
): Promise<void> {
  const url = new URL(req.url ?? '/', `http://127.0.0.1:${input.port}`)
  if (url.pathname === '/api/workflows/events') {
    const request = await nodeRequestToWebRequest(req, url)
    input.workflowEvents.push((await request.json()) as MockWorkflowEventRequest)
    await sendWebResponse(res, Response.json({ ok: true }))
    return
  }
  const match = /^\/api\/session\/([^/]+)(?:\/(messages|execute|events))?$/.exec(url.pathname)
  if (!match?.[1]) {
    await sendWebResponse(res, new Response('not found', { status: 404 }))
    return
  }
  const threadKey = decodeURIComponent(match[1])
  const endpoint = match[2] ?? 'session'

  if (endpoint === 'session') {
    const request = await nodeRequestToWebRequest(req, url)
    const body = (await request.json()) as SlackbotV2CreateSessionRequest
    input.creates.push({ threadKey, body })
    const queued = input.createResponses.shift()
    if (queued) {
      await sendWebResponse(res, Response.json(queued.body, { status: queued.status }))
      return
    }
    await sendWebResponse(
      res,
      Response.json({
        thread_key: threadKey,
        sandbox_id: null,
        harness_type: body.harness_type,
        harness_thread_id: null,
        harness_switched: false,
        persona_id: body.persona_id ?? null,
        status: 'active'
      })
    )
    return
  }

  if (endpoint === 'events') {
    const afterEventId = Number.parseInt(url.searchParams.get('after_event_id') ?? '0', 10) || 0
    const executionId = url.searchParams.get('execution_id') || undefined
    input.eventRequests.push({ threadKey, afterEventId, executionId })
    if (input.failNextEvents) {
      input.setFailNextEvents(false)
      await sendWebResponse(
        res,
        new Response('unavailable', { status: 503, statusText: 'Service Unavailable' })
      )
      return
    }
    res.writeHead(200, {
      'cache-control': 'no-cache',
      connection: 'keep-alive',
      'content-type': 'text/event-stream'
    })
    input.streams.add(res)
    for (const event of input.events) {
      if (
        event.threadKey === threadKey
        && event.id > afterEventId
        && (!executionId || !event.executionId || event.executionId === executionId)
      ) {
        writeMockSseEvent(res, event)
      }
    }
    req.once('close', () => {
      input.streams.delete(res)
    })
    return
  }

  const request = await nodeRequestToWebRequest(req, url)
  if (endpoint === 'messages') {
    const body = (await request.json()) as SlackbotV2AppendMessagesRequest
    input.appends.push({ threadKey, body })
    await sendWebResponse(res, Response.json({ ok: true, message_ids: body.messages.map((_, index) => `msg-${index + 1}`) }))
    return
  }

  const body = (await request.json()) as SlackbotV2ExecuteSessionRequest
  input.executes.push({ threadKey, body })
  if (input.failNextExecute) {
    input.setFailNextExecute(false)
    await sendWebResponse(res, new Response('unavailable', { status: 503, statusText: 'Service Unavailable' }))
    return
  }
  if (input.executeHold) await input.executeHold
  const idempotencyMapKey = body.idempotency_key
    ? `${threadKey}:${body.idempotency_key}`
    : undefined
  const existingExecutionId = idempotencyMapKey
    ? input.idempotentExecutions.get(idempotencyMapKey)
    : undefined
  const executionId =
    existingExecutionId ?? `exe-${input.idempotentExecutions.size + input.executes.length}`
  if (idempotencyMapKey && !existingExecutionId) {
    input.idempotentExecutions.set(idempotencyMapKey, executionId)
  }
  if (!existingExecutionId && input.autoRespond) {
    for (const line of sampleCodexOutputLines(`Executed request ${input.idempotentExecutions.size}.`)) {
      emitMockSessionEvent({
        data: line,
        event: 'session.output.line',
        executionId,
        events: input.events,
        id: input.nextEventId(),
        streams: input.streams,
        threadKey
      })
    }
  }
  if (input.failNextExecuteAfterAccept) {
    input.setFailNextExecuteAfterAccept(false)
    await sendWebResponse(
      res,
      new Response('response lost after accept', { status: 503, statusText: 'Service Unavailable' })
    )
    return
  }
  await sendWebResponse(
    res,
    Response.json({
      ok: true,
      execution_id: executionId,
      thread_key: threadKey,
      status: 'completed'
    })
  )
}

function emitMockSessionEvent(input: {
  data: string
  event: string
  executionId?: string
  events: MockSessionEvent[]
  id: number
  streams: Set<ServerResponse>
  threadKey: string
}): void {
  const event: MockSessionEvent = {
    data: input.data,
    event: input.event,
    executionId: input.executionId,
    id: input.id,
    threadKey: input.threadKey
  }
  input.events.push(event)
  for (const stream of input.streams) writeMockSseEvent(stream, event)
}

function writeMockSseEvent(stream: ServerResponse, event: MockSessionEvent): void {
  stream.write(`id: ${event.id}\n`)
  stream.write(`event: ${event.event}\n`)
  for (const line of event.data.split('\n')) {
    stream.write(`data: ${line}\n`)
  }
  stream.write('\n')
}

type PatchedSlackApi = {
  addFileToMessage(channel: string, ts: string, file: Record<string, unknown>): void
  botInfoRequestCount(botId: string): number
  calls: StreamCall[]
  close(): Promise<void>
  failRepliesWithThreadNotFound(channel: string, ts: string): void
  failStreamAppendsAfter(count: number, error: string): void
  failStreamStopsLongerThan(maxChars: number): void
  failNextStreamStop(): void
  fileInfoRequestCount(fileId: string): number
  holdAssistantStatus(): () => void
  reset(): void
  respondToNextConversationsJoin(status: number, body: Record<string, unknown>): void
  respondToNextReaction(status: number, body: Record<string, unknown>): void
  setBotInfo(botId: string, bot: Record<string, unknown>): void
  setFileInfo(fileId: string, file: Record<string, unknown>): void
  setUserProfile(userId: string, profile: Record<string, unknown>): void
  userProfileMethodRequestCount(userId: string, method: string): number
  userProfileRequestCount(userId: string): number
  url: string
}

type StreamCall = {
  body: Record<string, unknown>
  method:
    | 'agents.sessions.setStatus'
    | 'agents.sessions.rename'
    | 'assistant.threads.setStatus'
    | 'assistant.threads.setTitle'
    | 'chat.postMessage'
    | 'chat.update'
    | 'chat.startStream'
    | 'chat.appendStream'
    | 'chat.stopStream'
    | 'conversations.join'
    | 'reactions.add'
    | 'reactions.remove'
  streamTs?: string
}

type StreamRecord = {
  channel: string
  payloadChars: number
  text: string
  ts: string
}

type QueuedSlackApiResponse = {
  body: Record<string, unknown>
  status: number
}

type SlackStreamTranscript = {
  appends: StreamCall[]
  calls: StreamCall[]
  chunks: Record<string, unknown>[]
  start: StreamCall
  stop: StreamCall
  streamTs: string
}

async function startPatchedSlackApi(emulatorUrl: string): Promise<PatchedSlackApi> {
  const upstreamUrl = loopbackUrl(emulatorUrl)
  const calls: StreamCall[] = []
  const botInfo = new Map<string, Record<string, unknown>>()
  const botInfoRequests = new Map<string, number>()
  const fileInfo = new Map<string, Record<string, unknown>>()
  const fileInfoRequests = new Map<string, number>()
  const conversationsJoinResponses: QueuedSlackApiResponse[] = []
  const reactionResponses: QueuedSlackApiResponse[] = []
  const threadMessageFiles = new Map<string, Record<string, unknown>[]>()
  const userProfiles = new Map<string, Record<string, unknown>>()
  const userProfileRequests = new Map<string, number>()
  const threadNotFoundReplies = new Set<string>()
  let assistantStatusGate: Promise<void> | null = null
  let releaseAssistantStatusGate: (() => void) | null = null
  let maxStreamStopChars: number | null = null
  const stopFailure = { remaining: 0 }
  const appendFailure: { error: string; remaining: number } = { error: '', remaining: -1 }
  const streams = new Map<string, StreamRecord>()
  const releaseCurrentAssistantStatusGate = () => {
    const release = releaseAssistantStatusGate
    assistantStatusGate = null
    releaseAssistantStatusGate = null
    release?.()
  }
  const port = await availablePort(4053)
  const server = createServer((req, res) => {
    void handlePatchedSlackRequest(req, res, {
      appendFailure,
      assistantStatusGate: status => {
        if (!status) return null
        const gate = assistantStatusGate
        assistantStatusGate = null
        return gate
      },
      botInfo,
      botInfoRequests,
      calls,
      conversationsJoinResponses,
      fileInfo,
      fileInfoRequests,
      maxStreamStopChars,
      stopFailure,
      port,
      reactionResponses,
      streams,
      threadNotFoundReplies,
      threadMessageFiles,
      userProfiles,
      userProfileRequests,
      upstreamUrl
    }).catch(error => {
      res.writeHead(500, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ ok: false, error: String(error) }))
    })
  })
  await listen(server, port)
  return {
    addFileToMessage(channel: string, ts: string, file: Record<string, unknown>) {
      const key = slackReplyKey(channel, ts)
      threadMessageFiles.set(key, [...(threadMessageFiles.get(key) ?? []), file])
    },
    botInfoRequestCount(botId: string) {
      return botInfoRequests.get(botId) ?? 0
    },
    calls,
    url: `http://127.0.0.1:${port}`,
    failRepliesWithThreadNotFound(channel: string, ts: string) {
      threadNotFoundReplies.add(slackReplyKey(channel, ts))
    },
    failStreamAppendsAfter(count: number, error: string) {
      appendFailure.remaining = count
      appendFailure.error = error
    },
    failNextStreamStop() {
      stopFailure.remaining = 1
    },
    failStreamStopsLongerThan(maxChars: number) {
      maxStreamStopChars = maxChars
    },
    fileInfoRequestCount(fileId: string) {
      return fileInfoRequests.get(fileId) ?? 0
    },
    holdAssistantStatus() {
      if (releaseAssistantStatusGate) throw new Error('assistant status is already held')
      assistantStatusGate = new Promise(resolve => {
        releaseAssistantStatusGate = resolve
      })
      return releaseCurrentAssistantStatusGate
    },
    reset() {
      releaseCurrentAssistantStatusGate()
      calls.length = 0
      botInfo.clear()
      botInfoRequests.clear()
      maxStreamStopChars = null
      stopFailure.remaining = 0
      appendFailure.remaining = -1
      appendFailure.error = ''
      conversationsJoinResponses.length = 0
      reactionResponses.length = 0
      threadNotFoundReplies.clear()
      threadMessageFiles.clear()
      fileInfo.clear()
      fileInfoRequests.clear()
      streams.clear()
      userProfiles.clear()
      userProfileRequests.clear()
    },
    respondToNextConversationsJoin(status: number, body: Record<string, unknown>) {
      conversationsJoinResponses.push({ body, status })
    },
    respondToNextReaction(status: number, body: Record<string, unknown>) {
      reactionResponses.push({ body, status })
    },
    setBotInfo(botId: string, bot: Record<string, unknown>) {
      botInfo.set(botId, bot)
    },
    setFileInfo(fileId: string, file: Record<string, unknown>) {
      fileInfo.set(fileId, file)
    },
    setUserProfile(userId: string, profile: Record<string, unknown>) {
      userProfiles.set(userId, profile)
    },
    userProfileMethodRequestCount(userId: string, method: string) {
      return userProfileRequests.get(`${method}:${userId}`) ?? 0
    },
    userProfileRequestCount(userId: string) {
      return userProfileRequests.get(userId) ?? 0
    },
    close: () => closeServer(server)
  }
}

async function handlePatchedSlackRequest(
  req: IncomingMessage,
  res: ServerResponse,
  input: {
    appendFailure: { error: string; remaining: number }
    assistantStatusGate: (status: string) => Promise<void> | null
    botInfo: Map<string, Record<string, unknown>>
    botInfoRequests: Map<string, number>
    calls: StreamCall[]
    conversationsJoinResponses: QueuedSlackApiResponse[]
    fileInfo: Map<string, Record<string, unknown>>
    fileInfoRequests: Map<string, number>
    maxStreamStopChars: number | null
    stopFailure: { remaining: number }
    port: number
    reactionResponses: QueuedSlackApiResponse[]
    streams: Map<string, StreamRecord>
    threadNotFoundReplies: Set<string>
    threadMessageFiles: Map<string, Record<string, unknown>[]>
    userProfiles: Map<string, Record<string, unknown>>
    userProfileRequests: Map<string, number>
    upstreamUrl: string
  }
): Promise<void> {
  const url = new URL(req.url ?? '/', `http://127.0.0.1:${input.port}`)
  const request = await nodeRequestToWebRequest(req, url)

  if (url.pathname.endsWith('/files/captured.png') || url.pathname.endsWith('/captured.png')) {
    await sendWebResponse(
      res,
      new Response('captured-image', {
        headers: { 'content-type': 'image/png' }
      })
    )
    return
  }

  if (
    url.pathname.endsWith('/files/large-upload.mp4')
    || url.pathname.endsWith('/large-upload.mp4')
  ) {
    await sendWebResponse(
      res,
      new Response(new Uint8Array(2 * 1024 * 1024), {
        headers: { 'content-type': 'video/mp4' }
      })
    )
    return
  }

  const path = normalizeApiPath(url.pathname)
  if (path === '/api/agents.sessions.setStatus' || path === '/api/agents.sessions.rename') {
    const body = await requestBody(request)
    input.calls.push({ method: path.slice('/api/'.length) as StreamCall['method'], body })
    await sendWebResponse(res, Response.json({ ok: true }))
    return
  }
  if (path === '/api/assistant.threads.setStatus') {
    const body = await requestBody(request)
    input.calls.push({ method: 'assistant.threads.setStatus', body })
    const gate = input.assistantStatusGate(stringField(body.status))
    if (gate) await gate
    await sendWebResponse(res, Response.json({ ok: true }))
    return
  }
  if (path === '/api/assistant.threads.setTitle') {
    const body = await requestBody(request)
    input.calls.push({ method: 'assistant.threads.setTitle', body })
    await sendWebResponse(res, Response.json({ ok: true }))
    return
  }
  if (path === '/api/users.info' || path === '/api/users.profile.get') {
    const userId = url.searchParams.get('user') ?? stringField((await requestBody(request)).user)
    input.userProfileRequests.set(userId, (input.userProfileRequests.get(userId) ?? 0) + 1)
    input.userProfileRequests.set(path, (input.userProfileRequests.get(path) ?? 0) + 1)
    input.userProfileRequests.set(`${path}:${userId}`, (input.userProfileRequests.get(`${path}:${userId}`) ?? 0) + 1)
    const profile = input.userProfiles.get(userId) ?? (userId === BOT_USER_ID ? {
      name: 'centaur', real_name: 'centaur', fields: {}
    } : {
      name: 'tester',
      real_name: 'Test User',
      fields: {}
    })
    if (path === '/api/users.info') {
      await sendWebResponse(
        res,
        Response.json({
          ok: true,
          user: {
            id: userId,
            name: profile.name,
            real_name: profile.real_name,
            profile
          }
        })
      )
      return
    }
    await sendWebResponse(res, Response.json({ ok: true, profile }))
    return
  }
  if (path === '/api/bots.info') {
    const botId = url.searchParams.get('bot') ?? stringField((await requestBody(request)).bot)
    input.botInfoRequests.set(botId, (input.botInfoRequests.get(botId) ?? 0) + 1)
    const bot = input.botInfo.get(botId)
    await sendWebResponse(
      res,
      bot
        ? Response.json({ ok: true, bot })
        : Response.json({ ok: false, error: 'bot_not_found' })
    )
    return
  }
  if (path === '/api/files.info') {
    const body = await requestBody(request.clone())
    const fileId = url.searchParams.get('file') ?? stringField(body.file)
    input.fileInfoRequests.set(fileId, (input.fileInfoRequests.get(fileId) ?? 0) + 1)
    const file = input.fileInfo.get(fileId)
    await sendWebResponse(
      res,
      file
        ? Response.json({ ok: true, file })
        : Response.json({ ok: false, error: 'file_not_found' })
    )
    return
  }
  if (path === '/api/conversations.join') {
    const body = await requestBody(request)
    input.calls.push({ method: 'conversations.join', body })
    const configuredResponse = input.conversationsJoinResponses.shift()
    if (configuredResponse) {
      await sendWebResponse(
        res,
        Response.json(configuredResponse.body, { status: configuredResponse.status })
      )
      return
    }
    await sendWebResponse(
      res,
      Response.json({
        ok: true,
        channel: {
          id: stringField(body.channel),
          is_channel: true
        }
      })
    )
    return
  }
  if (path === '/api/reactions.add' || path === '/api/reactions.remove') {
    const body = await requestBody(request)
    input.calls.push({
      method: path === '/api/reactions.add' ? 'reactions.add' : 'reactions.remove',
      body
    })
    const configuredResponse = input.reactionResponses.shift()
    await sendWebResponse(
      res,
      configuredResponse
        ? Response.json(configuredResponse.body, { status: configuredResponse.status })
        : Response.json({ ok: true })
    )
    return
  }
  if (path === '/api/chat.postMessage' || path === '/api/chat.update') {
    const body = await requestBody(request.clone())
    if (typeof body.markdown_text === 'string') {
      input.calls.push({
        method: path === '/api/chat.postMessage' ? 'chat.postMessage' : 'chat.update',
        body
      })
      if (body.markdown_text.length > SLACK_MARKDOWN_TEXT_MAX_CHARS) {
        await sendWebResponse(res, Response.json({ ok: false, error: 'msg_too_long' }))
        return
      }
      const { markdown_text: markdownText, ...legacyBody } = body
      await sendWebResponse(
        res,
        Response.json(
          await postSlack(input.upstreamUrl, request, path, {
            ...legacyBody,
            text: markdownText
          })
        )
      )
      return
    }
  }
  if (path === '/api/chat.startStream') {
    await sendWebResponse(
      res,
      await startStream(input.upstreamUrl, request, input.streams, input.calls)
    )
    return
  }
  if (path === '/api/chat.appendStream') {
    await sendWebResponse(
      res,
      await appendStream(input.upstreamUrl, request, input.streams, input.calls, input.appendFailure)
    )
    return
  }
  if (path === '/api/chat.stopStream') {
    await sendWebResponse(
      res,
      await stopStream(
        input.upstreamUrl,
        request,
        input.streams,
        input.calls,
        input.maxStreamStopChars,
        input.stopFailure
      )
    )
    return
  }
  if (path === '/api/conversations.replies') {
    const body = await requestBody(request.clone())
    if (
      input.threadNotFoundReplies.has(
        slackReplyKey(stringField(body.channel), stringField(body.ts))
      )
    ) {
      await sendWebResponse(res, Response.json({ ok: false, error: 'thread_not_found' }))
      return
    }
    if (input.threadMessageFiles.size > 0) {
      const rawBody = await request.arrayBuffer()
      const proxied = await fetch(new URL(`${path}${url.search}`, input.upstreamUrl), {
        method: request.method,
        headers: request.headers,
        body: rawBody.byteLength > 0 ? rawBody : undefined
      })
      const payload = await proxied.json() as Record<string, unknown>
      if (Array.isArray(payload.messages)) {
        payload.messages = payload.messages.map(message => {
          if (!message || typeof message !== 'object' || Array.isArray(message)) return message
          const item = message as Record<string, unknown>
          const files = input.threadMessageFiles.get(
            slackReplyKey(stringField(body.channel), stringField(item.ts))
          )
          return files ? { ...item, files: [...slackFileArray(item.files), ...files] } : item
        })
      }
      await sendWebResponse(res, Response.json(payload, { status: proxied.status }))
      return
    }
  }

  const body = await request.arrayBuffer()
  const proxied = await fetch(new URL(`${path}${url.search}`, input.upstreamUrl), {
    method: request.method,
    headers: request.headers,
    body: body.byteLength > 0 ? body : undefined
  })
  await sendWebResponse(res, proxied)
}

function loopbackUrl(value: string): string {
  const url = new URL(value)
  url.hostname = '127.0.0.1'
  return url.toString()
}

async function nodeRequestToWebRequest(
  req: IncomingMessage,
  url: URL
): Promise<Request> {
  const headers = new Headers()
  for (const [key, value] of Object.entries(req.headers)) {
    if (Array.isArray(value)) {
      for (const item of value) headers.append(key, item)
    } else if (typeof value === 'string') {
      headers.set(key, value)
    }
  }

  const chunks: Buffer[] = []
  for await (const chunk of req) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk))
  }
  const body = Buffer.concat(chunks)
  return new Request(url, {
    body: body.length > 0 && req.method !== 'GET' && req.method !== 'HEAD' ? body : undefined,
    headers,
    method: req.method
  })
}

async function sendWebResponse(res: ServerResponse, response: Response): Promise<void> {
  res.statusCode = response.status
  res.statusMessage = response.statusText
  response.headers.forEach((value, key) => {
    // The proxy buffers the body, so let Node choose framing for the buffered response.
    if (key === 'transfer-encoding') return
    res.setHeader(key, value)
  })
  if (response.body === null || response.status === 204) {
    res.end()
    return
  }
  res.end(Buffer.from(await response.arrayBuffer()))
}

function listen(server: HttpServer, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(port, '127.0.0.1', () => {
      server.off('error', reject)
      resolve()
    })
  })
}

function closeServer(server: HttpServer): Promise<void> {
  return new Promise((resolve, reject) => {
    server.close(error => {
      if (error) reject(error)
      else resolve()
    })
  })
}

async function startStream(
  emulatorUrl: string,
  request: Request,
  streams: Map<string, StreamRecord>,
  calls: StreamCall[]
): Promise<Response> {
  const body = await requestBody(request)
  const channel = stringField(body.channel)
  const threadTs = stringField(body.thread_ts)
  const text = streamBodyText(body) || ' '
  const payloadChars = streamBodyPayloadChars(body)
  const posted = await postSlack(emulatorUrl, request, '/api/chat.postMessage', {
    channel,
    thread_ts: threadTs || undefined,
    text
  })
  if (!posted.ok) return Response.json(posted)
  const ts = stringField(posted.ts)
  calls.push({ method: 'chat.startStream', body, streamTs: ts })
  streams.set(streamKey(channel, ts), { channel, payloadChars, ts, text })
  return Response.json({ ok: true, channel, ts })
}

async function appendStream(
  emulatorUrl: string,
  request: Request,
  streams: Map<string, StreamRecord>,
  calls: StreamCall[],
  appendFailure: { error: string; remaining: number }
): Promise<Response> {
  const body = await requestBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  calls.push({ method: 'chat.appendStream', body, streamTs: ts })
  if (appendFailure.remaining === 0) {
    // The stream broke server-side: real Slack renders the message as
    // "Something went wrong" and drops the streamed content.
    await postSlack(emulatorUrl, request, '/api/chat.update', {
      channel,
      ts,
      text: BROKEN_STREAM_TEXT
    })
    return Response.json({ ok: false, error: appendFailure.error })
  }
  if (appendFailure.remaining > 0) appendFailure.remaining -= 1
  const record = streams.get(streamKey(channel, ts)) ?? { channel, payloadChars: 0, ts, text: '' }
  record.text += streamBodyText(body)
  record.payloadChars += streamBodyPayloadChars(body)
  streams.set(streamKey(channel, ts), record)
  await postSlack(emulatorUrl, request, '/api/chat.update', {
    channel,
    ts,
    text: record.text || ' '
  })
  return Response.json({ ok: true, channel, ts })
}

async function stopStream(
  emulatorUrl: string,
  request: Request,
  streams: Map<string, StreamRecord>,
  calls: StreamCall[],
  maxStreamStopChars: number | null,
  stopFailure: { remaining: number }
): Promise<Response> {
  const body = await requestBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  calls.push({ method: 'chat.stopStream', body, streamTs: ts })
  if (stopFailure.remaining > 0) {
    stopFailure.remaining -= 1
    return Response.json({ ok: false, error: 'internal_error' })
  }
  const key = streamKey(channel, ts)
  const record = streams.get(key) ?? { channel, payloadChars: 0, ts, text: '' }
  const text = [record.text, streamBodyText(body)].filter(part => part.trim()).join('\n')
  const payloadChars = record.payloadChars + streamBodyPayloadChars(body)
  if (maxStreamStopChars !== null && payloadChars > maxStreamStopChars) {
    // A stream that is never stopped breaks in real Slack: the message shows
    // "Something went wrong" instead of the streamed content.
    await postSlack(emulatorUrl, request, '/api/chat.update', {
      channel,
      ts,
      text: BROKEN_STREAM_TEXT
    })
    return Response.json({ ok: false, error: 'msg_too_long' })
  }
  await postSlack(emulatorUrl, request, '/api/chat.update', {
    channel,
    ts,
    text: text || record.text || ' '
  })
  streams.delete(key)
  return Response.json({ ok: true, channel, ts })
}

async function requestBody(request: Request): Promise<Record<string, unknown>> {
  const raw = await request.text()
  const contentType = request.headers.get('content-type') ?? ''
  if (contentType.includes('application/json')) return JSON.parse(raw || '{}')
  return Object.fromEntries(
    Array.from(new URLSearchParams(raw).entries()).map(([key, value]) => [
      key,
      parseMaybeJson(value)
    ])
  )
}

async function postSlack(
  emulatorUrl: string,
  original: Request,
  path: string,
  body: Record<string, unknown>
): Promise<Record<string, unknown>> {
  const response = await fetch(new URL(path, emulatorUrl), {
    method: 'POST',
    headers: {
      authorization: original.headers.get('authorization') ?? '',
      'content-type': 'application/json'
    },
    body: JSON.stringify(body)
  })
  return (await response.json()) as Record<string, unknown>
}

function streamBodyText(body: Record<string, unknown>): string {
  return [stringField(body.markdown_text), chunksText(body.chunks)].filter(Boolean).join('\n')
}

function streamBodyPayloadChars(body: Record<string, unknown>): number {
  return (
    stringField(body.markdown_text).length
    + JSON.stringify(streamChunks(body.chunks)).length
    + JSON.stringify(body.blocks ?? []).length
  )
}

function streamChunks(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return []
  return value.filter((chunk): chunk is Record<string, unknown> => {
    return Boolean(chunk) && typeof chunk === 'object' && !Array.isArray(chunk)
  })
}

function expectSlackPlanStreamShape(
  calls: StreamCall[],
  input: {
    answers: string[]
    parentTs: string
  }
): void {
  const transcripts = slackStreamTranscripts(calls)
  expect(transcripts).toHaveLength(input.answers.length)

  for (const [index, transcript] of transcripts.entries()) {
    const answer = input.answers[index]!
    const markdownChunks = transcript.chunks.filter(chunk => chunk.type === 'markdown_text')
    const progressChunks = transcript.chunks.filter(chunk => chunk.type !== 'markdown_text')
    const markdownText = markdownChunks.map(chunk => stringField(chunk.text)).join('')
    const progressText = progressChunks.map(chunkText).filter(Boolean).join('\n')
    const renderedText = transcript.chunks.map(chunkText).filter(Boolean).join('\n')
    const markdownIndex = transcript.chunks.findIndex(chunk => chunk.type === 'markdown_text')

    expect(transcript.start.body).toEqual(
      expect.objectContaining({
        channel: CHANNEL_ID,
        thread_ts: input.parentTs,
        recipient_user_id: USER_ID,
        recipient_team_id: TEAM_ID,
        task_display_mode: 'plan'
      })
    )
    expect(transcript.start.body.ts).toBeUndefined()
    expect(transcript.start.body.markdown_text).toBeUndefined()

    for (const append of transcript.appends) {
      expect(append.body).toEqual(
        expect.objectContaining({
          channel: CHANNEL_ID,
          ts: transcript.streamTs
        })
      )
      expect(append.body.thread_ts).toBeUndefined()
      expect(append.body.recipient_user_id).toBeUndefined()
      expect(append.body.recipient_team_id).toBeUndefined()
      expect(append.body.task_display_mode).toBeUndefined()
      expect(append.body.markdown_text).toBeUndefined()
      expect(streamChunks(append.body.chunks).length).toBeGreaterThan(0)
    }

    expect(transcript.stop.body).toEqual(
      expect.objectContaining({
        channel: CHANNEL_ID,
        ts: transcript.streamTs
      })
    )
    expect(transcript.stop.body.thread_ts).toBeUndefined()
    expect(transcript.stop.body.recipient_user_id).toBeUndefined()
    expect(transcript.stop.body.recipient_team_id).toBeUndefined()
    expect(transcript.stop.body.task_display_mode).toBeUndefined()
    const stopFinalText = [
      stringField(transcript.stop.body.markdown_text),
      blocksText(transcript.stop.body.blocks)
    ]
      .filter(Boolean)
      .join('\n')
    if (stopFinalText) expect(stopFinalText).toContain(answer)

    expect(markdownChunks).toEqual([{ type: 'markdown_text', text: answer }])
    expect(markdownText).toBe(answer)
    expect(markdownText).not.toContain('Implementation plan')
    expect(markdownText).not.toContain('Checking the command output')
    expect(markdownText).not.toContain('Inspecting the event stream')
    expect(markdownText).not.toContain('Thinking')
    expect(markdownText).not.toContain('Command execution')
    expect(markdownText).not.toContain('pnpm test')
    expect(markdownText).not.toContain('tests passed')
    expect(progressText).not.toContain(answer)

    expect(markdownIndex).toBe(transcript.chunks.length - 1)
    expect(progressChunks.length).toBeGreaterThan(0)
    expect(progressChunks.every(chunk =>
      chunk.type === 'plan_update' || chunk.type === 'task_update'
    )).toBe(true)

    expect(progressChunks).toContainEqual(
      expect.objectContaining({ type: 'plan_update', title: 'Implementation plan' })
    )
    expect(
      progressChunks.some(chunk => chunk.type === 'task_update' && chunk.title === 'Thinking')
    ).toBe(false)
    expect(progressText).not.toContain('Checking the command output')
    expect(progressText).not.toContain('Inspecting the event stream')
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'cmd-1',
        title: '1. Command execution',
        details: expect.stringContaining('pnpm test')
      })
    )
    const commandChunk = progressChunks.find(
      chunk => chunk.type === 'task_update' && chunk.id === 'cmd-1'
    )
    expect(commandChunk).toBeDefined()
    expect(
      progressChunks
        .filter(chunk => chunk.type === 'task_update')
        .every(chunk => stringField(chunk.output) === '')
    ).toBe(true)

    expect(renderedText).toContain('Implementation plan')
    expect(renderedText).toContain('Inspect App Server events')
    expect(renderedText).toContain('Stream Chat SDK chunks')
    expect(renderedText).not.toContain('Checking the command output')
    expect(renderedText).not.toContain('Inspecting the event stream')
    expect(renderedText).not.toContain('Thinking')
    expect(renderedText).toContain('Command execution')
    expect(renderedText).toContain('pnpm test')
    expect(renderedText).not.toContain('tests passed')
    expect(renderedText.trim().endsWith(answer)).toBe(true)
  }
}

function expectSlackRenderedReply(text: string, answer: string): void {
  expect(text).toContain('Implementation plan')
  expect(text).toContain('Inspect App Server events')
  expect(text).toContain('Stream Chat SDK chunks')
  expect(text).not.toContain('Thinking')
  expect(text).not.toContain('Checking the command output')
  expect(text).not.toContain('Inspecting the event stream')
  expect(text).toContain('Command execution')
  expect(text).toContain('pnpm test')
  expect(text).not.toContain('tests passed')
  expect(text.trim().endsWith(answer)).toBe(true)
}

function slackStreamTranscripts(calls: StreamCall[]): SlackStreamTranscript[] {
  const starts = calls.filter((call): call is StreamCall & { streamTs: string } => {
    return call.method === 'chat.startStream' && Boolean(call.streamTs)
  })

  return starts.map(start => {
    const streamTs = start.streamTs
    const streamCalls = calls.filter(call => {
      if (call === start) return true
      if (call.method !== 'chat.appendStream' && call.method !== 'chat.stopStream') return false
      return stringField(call.body.ts) === streamTs
    })
    const appends = streamCalls.filter(call => call.method === 'chat.appendStream')
    const stops = streamCalls.filter(call => call.method === 'chat.stopStream')
    expect(stops).toHaveLength(1)
    const stop = stops[0]!
    const chunks = streamCalls.flatMap(call => streamChunks(call.body.chunks))
    return { appends, calls: streamCalls, chunks, start, stop, streamTs }
  })
}

function streamTranscriptPayloadChars(transcript: SlackStreamTranscript): number {
  return transcript.calls.reduce((total, call) => total + streamBodyPayloadChars(call.body), 0)
}

function chunkText(chunk: Record<string, unknown>): string {
  if (typeof chunk.text === 'string') return chunk.text
  return [chunk.title, chunk.details, chunk.output]
    .filter(part => typeof part === 'string' && part.trim())
    .join('\n')
}

function chunksText(value: unknown): string {
  return streamChunks(value)
    .map(chunkText)
    .filter(Boolean)
    .join('\n')
}

function blocksText(value: unknown): string {
  if (!Array.isArray(value)) return ''
  return value
    .map(block => {
      if (!block || typeof block !== 'object' || Array.isArray(block)) return ''
      const text = (block as Record<string, unknown>).text
      if (typeof text === 'string') return text
      if (!text || typeof text !== 'object' || Array.isArray(text)) return ''
      return stringField((text as Record<string, unknown>).text)
    })
    .filter(Boolean)
    .join('\n')
}

function stopStreamBlocksText(calls: StreamCall[]): string {
  const blocks = calls
    .filter(call => call.method === 'chat.stopStream')
    .flatMap(call => (Array.isArray(call.body.blocks) ? call.body.blocks : []))
  const elements = blocks.flatMap(block => {
    if (!block || typeof block !== 'object' || Array.isArray(block)) return []
    const value = (block as Record<string, unknown>).elements
    return Array.isArray(value) ? value : []
  })
  return blocksText([...blocks, ...elements])
}

function normalizeApiPath(path: string): string {
  return path.startsWith('/api/') ? path : `/api${path}`
}

function streamKey(channel: string, ts: string): string {
  return `${channel}:${ts}`
}

function slackReplyKey(channel: string, ts: string): string {
  return `${channel}:${ts}`
}

function incrementSlackTs(ts: string, seconds: number): string {
  const [whole = '0', fractional = '000000'] = ts.split('.')
  return `${Number.parseInt(whole, 10) + seconds}.${fractional.padEnd(6, '0').slice(0, 6)}`
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function slackFileArray(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? (value.filter(item =>
        item && typeof item === 'object' && !Array.isArray(item)
      ) as Record<string, unknown>[])
    : []
}

function parseMaybeJson(value: string): unknown {
  const trimmed = value.trim()
  if (!trimmed || !['[', '{'].includes(trimmed[0] ?? '')) return value
  try {
    return JSON.parse(trimmed)
  } catch {
    return value
  }
}

async function waitFor(
  predicate: () => boolean | Promise<boolean>,
  timeoutMs = 1000
): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await predicate()) return
    await new Promise(resolve => setTimeout(resolve, 10))
  }
  throw new Error('Timed out waiting for condition')
}

async function sleep(ms: number): Promise<void> {
  await new Promise(resolve => setTimeout(resolve, ms))
}

async function availablePort(preferred: number): Promise<number> {
  for (let port = preferred; port < preferred + 100; port++) {
    if (!(await isPortOpen(port))) return port
  }
  throw new Error(`No available port near ${preferred}`)
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

// Regression coverage for the 2026-07-06 incident: every execute turn opens a
// GET /api/session/{key}/events SSE stream; abandoning it after the terminal
// event without cancelling the reader leaked one connection per turn. At
// Bun's global fetch cap (BUN_CONFIG_MAX_HTTP_REQUESTS, default 256) every
// outbound fetch queued forever and all handoffs failed. parseSseEvents now
// cancels the reader when the consumer stops, so connections are released.
describe('session event stream connection lifecycle', () => {
  function openEventStreamGauge(): number {
    const match = /^slackbotv2_session_event_streams_open (\d+)$/m.exec(slackbotMetrics.expose())
    return match?.[1] ? Number.parseInt(match[1], 10) : 0
  }

  function cancelledClosures(): number {
    const match = /^slackbotv2_session_event_stream_closures_total\{reason="cancelled"\} (\d+)$/m
      .exec(slackbotMetrics.expose())
    return match?.[1] ? Number.parseInt(match[1], 10) : 0
  }

  async function waitForGaugeAtMost(limit: number): Promise<number> {
    const deadline = Date.now() + 5_000
    let open = openEventStreamGauge()
    while (open > limit && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 10))
      open = openEventStreamGauge()
    }
    return open
  }

  async function deliverMention(bot: SlackbotV2, turn: number, threadTs: string, ts: string) {
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: `Ev-stream-lifecycle-${threadTs}-${turn}`,
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts,
          thread_ts: threadTs,
          text: `<@${BOT_USER_ID}> stream lifecycle turn ${turn}`
        }
      }),
      {},
      waitUntilContext(waits)
    )
    await Promise.all(waits).catch(() => {})
    return response
  }

  it('cancels the events connection once a turn reaches its terminal event', async () => {
    const gaugeBefore = openEventStreamGauge()
    const closuresBefore = cancelledClosures()
    const parent = await postUserMessage('Stream lifecycle thread.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> stream lifecycle turn 0`, parent.ts)

    const response = await deliverMention(bot, 0, parent.ts, mention.ts)
    expect(response.status).toBe(200)

    const open = await waitForGaugeAtMost(gaugeBefore)
    expect(open).toBeLessThanOrEqual(gaugeBefore)
    expect(cancelledClosures()).toBeGreaterThan(closuresBefore)
  })

  // Drives past Bun's 256-request cap to prove the wedge is gone. `bun test`
  // ignores the BUN_CONFIG_MAX_HTTP_REQUESTS override but still enforces the
  // built-in 256 cap, so crossing 260 turns exercises the real limit. Takes
  // ~30s, so it is opt-in:
  //
  //   SLACKBOTV2_POOL_WEDGE_REPRO=1 bun test test/chat-sdk-emulate.test.ts -t 'pool cap'
  const runCapCrossing = process.env.SLACKBOTV2_POOL_WEDGE_REPRO === '1' ? it : it.skip
  runCapCrossing(
    'keeps handing off past the 256-connection pool cap without wedging',
    async () => {
      const poolCap = 256
      const gaugeBefore = openEventStreamGauge()
      const repro = createTestBot({
        sessionApiTimeoutMs: 5_000,
        slackApiTimeoutMs: 2_000
      })

      let peak = 0
      for (let turn = 0; turn < poolCap + 4; turn++) {
        // One thread per turn, like production traffic: reusing a single
        // thread makes per-turn context collection grow quadratically.
        const parent = await postUserMessage(`Pool cap crossing thread ${turn + 1}.`)
        const mention = await postUserMessage(
          `<@${BOT_USER_ID}> stream lifecycle turn ${turn + 1}`,
          parent.ts
        )
        const response = await deliverMention(repro, turn + 1, parent.ts, mention.ts)
        expect(response.status).toBe(200)
        const open = await waitForGaugeAtMost(gaugeBefore)
        peak = Math.max(peak, open)
        if ((turn + 1) % 64 === 0 || turn >= poolCap) {
          console.log(`turn ${turn + 1}/${poolCap + 4}: ok, open streams settled at ${open}`)
        }
        expect(open).toBeLessThanOrEqual(gaugeBefore)
      }
      console.log(`peak settled gauge: ${peak}; cancelled closures: ${cancelledClosures()}`)
    },
    480_000
  )
})
