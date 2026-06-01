import { createHmac } from 'node:crypto'
import {
  createServer,
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
  type SlackbotV2,
  type SlackbotV2AppendMessagesRequest,
  type SlackbotV2CreateSessionRequest,
  type SlackbotV2ExecuteSessionRequest,
  type SlackbotV2SessionMessage
} from '../src/index'

const BOT_TOKEN = 'xoxb-slackbotv2-emulate'
const USER_TOKEN = 'xoxp-slackbotv2-user'
const SIGNING_SECRET = 'slackbotv2-signing-secret'
const BOT_USER_ID = 'U000000001'
const USER_ID = 'USLACKBOTV2USER'
const TEAM_ID = 'T000000001'
const CHANNEL_ID = 'C000000001'

let emulator: Emulator
let slackApi: PatchedSlackApi
let codexApi: MockSessionApi
let slack: WebClient
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
          scopes: ['assistant:write', 'chat:write', 'channels:read', 'users:read']
        },
        [USER_TOKEN]: {
          login: USER_ID,
          scopes: ['chat:write', 'channels:read', 'users:read']
        }
      },
      slack: {
        team: { name: 'Slackbot V2', domain: 'slackbot-v2' },
        users: [{ name: 'tester', real_name: 'Test User', email: 'tester@example.com' }],
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
})

beforeEach(() => {
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
  it('syncs thread context, forwards subscribed messages, and renders execute streams', async () => {
    const parent = await postUserMessage('The deploy context is above.')
    const firstMention = await postUserMessage(
      `<@${BOT_USER_ID}> run with this screenshot`,
      parent.ts
    )
    const fileUrl = `${slackApi.url}/files/captured.png`
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-first-mention',
        event: {
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
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

    const followUp = await postUserMessage('Additional detail for the subscribed thread.', parent.ts)
    const followUpWaits: Promise<unknown>[] = []
    const followUpResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-follow-up',
        event: {
          type: 'message',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: followUp.ts,
          thread_ts: parent.ts,
          text: 'Additional detail for the subscribed thread.'
        }
      }),
      {},
      waitUntilContext(followUpWaits)
    )

    expect(followUpResponse.status).toBe(200)
    await Promise.all(followUpWaits)

    const secondMention = await postUserMessage(`<@${BOT_USER_ID}> now execute with the latest`, parent.ts)
    const secondMentionWaits: Promise<unknown>[] = []
    const secondMentionResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-second-mention',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: secondMention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> now execute with the latest`
        }
      }),
      {},
      waitUntilContext(secondMentionWaits)
    )

    expect(secondMentionResponse.status).toBe(200)
    await Promise.all(secondMentionWaits)

    expect(codexApi.appends).toHaveLength(3)
    expect(codexApi.creates.map(create => create.threadKey)).toEqual([
      threadKey(parent.ts),
      threadKey(parent.ts),
      threadKey(parent.ts)
    ])
    expect(codexApi.executes).toHaveLength(2)

    const firstAppend = codexApi.appends[0]!
    expect(firstAppend.threadKey).toBe(threadKey(parent.ts))
    expect(sessionMessageTexts(firstAppend.body.messages)).toContain('The deploy context is above.')
    expect(sessionMessageTexts(firstAppend.body.messages).some(text =>
      text.includes('run with this screenshot')
    )).toBe(true)
    const firstAttachment = firstAppend.body.messages
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

    const firstExecute = codexApi.executes[0]!
    expect(firstExecute.threadKey).toBe(threadKey(parent.ts))
    const firstInputLine = JSON.parse(firstExecute.body.input_lines[0]!) as Record<string, unknown>
    expect(firstInputLine).toEqual(
      expect.objectContaining({
        type: 'user',
        thread_key: threadKey(parent.ts)
      })
    )
    expect(JSON.stringify(firstInputLine)).toContain('data:image/png;base64')

    const followUpAppend = codexApi.appends[1]!
    expect(followUpAppend.threadKey).toBe(threadKey(parent.ts))
    expect(sessionMessageTexts(followUpAppend.body.messages)).toEqual([
      'Additional detail for the subscribed thread.'
    ])

    const secondMentionAppend = codexApi.appends[2]!
    expect(sessionMessageTexts(secondMentionAppend.body.messages)[0]).toContain(
      'now execute with the latest'
    )
    const secondExecute = codexApi.executes[1]!
    expect(JSON.stringify(JSON.parse(secondExecute.body.input_lines[0]!))).toContain(
      'now execute with the latest'
    )

    expectSlackPlanStreamShape(slackApi.calls, {
      answers: ['Executed request 1.', 'Executed request 2.'],
      parentTs: parent.ts
    })
    const assistantStatuses = slackApi.calls
      .filter(call => call.method === 'assistant.threads.setStatus')
      .map(call => stringField(call.body.status))
    expect(assistantStatuses).toEqual(['Thinking...', '', 'Thinking...', ''])
    expect(
      slackApi.calls
        .filter(call => call.method === 'assistant.threads.setTitle')
        .map(call => stringField(call.body.title))
    ).toEqual([
      'run with this screenshot',
      'Codex request 1',
      'now execute with the latest',
      'Codex request 2'
    ])

    const text = await threadText(parent.ts)
    expect(text).toContain('Implementation plan')
    expect(text).toContain('Inspect App Server events')
    expect(text).toContain('Checking the command output')
    expect(text).toContain('Inspecting the event stream')
    expect(text).toContain('Command execution')
    expect(text).toContain('pnpm test')
    expect(text).toContain('tests passed')
    expect(text).toContain('Executed request 1.')
    expect(text).toContain('Executed request 2.')

    const renderedReplies = (await threadTexts(parent.ts)).filter(reply =>
      reply.includes('Executed request')
    )
    expect(renderedReplies).toHaveLength(2)
    expectSlackRenderedReply(renderedReplies[0]!, 'Executed request 1.')
    expectSlackRenderedReply(renderedReplies[1]!, 'Executed request 2.')
  })

  it('forwards subscribed messages to /messages without executing during a stream', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before the long run.')
    const firstMention = await postUserMessage(`<@${BOT_USER_ID}> start a long run`, parent.ts)
    const firstWaits: Promise<unknown>[] = []
    const firstResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-long-run',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: firstMention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> start a long run`
        }
      }),
      {},
      waitUntilContext(firstWaits)
    )
    expect(firstResponse.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const followUp = await postUserMessage('Actually queue this extra constraint.', parent.ts)
    const followUpWaits: Promise<unknown>[] = []
    const followUpResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-follow-up-during-stream',
        event: {
          type: 'message',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: followUp.ts,
          thread_ts: parent.ts,
          text: 'Actually queue this extra constraint.'
        }
      }),
      {},
      waitUntilContext(followUpWaits)
    )

    expect(followUpResponse.status).toBe(200)
    await Promise.all(followUpWaits)
    expect(codexApi.appends).toHaveLength(2)
    expect(codexApi.executes).toHaveLength(1)
    expect(sessionMessageTexts(codexApi.appends[1]!.body.messages)).toEqual([
      'Actually queue this extra constraint.'
    ])

    codexApi.closeStreams()
    await Promise.all(firstWaits)
  })

  it('does not execute a second mention while a stream is already active', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before the long mention run.')
    const firstMention = await postUserMessage(`<@${BOT_USER_ID}> start a long run`, parent.ts)
    const firstWaits: Promise<unknown>[] = []
    const firstResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-long-mention-run',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: firstMention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> start a long run`
        }
      }),
      {},
      waitUntilContext(firstWaits)
    )
    expect(firstResponse.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const secondMentionText = `<@${BOT_USER_ID}> add this while still running`
    const secondMention = await postUserMessage(secondMentionText, parent.ts)
    const secondWaits: Promise<unknown>[] = []
    const secondResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-second-mention-during-stream',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: secondMention.ts,
          thread_ts: parent.ts,
          text: secondMentionText
        }
      }),
      {},
      waitUntilContext(secondWaits)
    )

    expect(secondResponse.status).toBe(200)
    await Promise.all(secondWaits)
    await waitFor(() => codexApi.appends.length === 2)
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.streamCount).toBe(1)
    expect(sessionMessageTexts(codexApi.appends[1]!.body.messages)).toEqual([
      `@${BOT_USER_ID} add this while still running`
    ])

    codexApi.closeStreams()
    await Promise.all(firstWaits)
  })

  it('starts the Slack stream before a slow session execute returns', async () => {
    codexApi.autoRespond = false
    const releaseExecute = codexApi.holdNextExecute()

    const parent = await postUserMessage('Context before the slow run.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> start visibly`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-slow-execute',
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

    expect(response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => slackApi.calls.some(call => call.method === 'chat.startStream'))
    expect(codexApi.eventRequests).toHaveLength(0)

    releaseExecute()
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)
    codexApi.closeStreams()
    await Promise.all(waits)
  })

  it('refetches full context on a later mention if the initial execute failed', async () => {
    codexApi.failNextExecute = true

    const parent = await postUserMessage('History that must not be lost.')
    const failedMention = await postUserMessage(`<@${BOT_USER_ID}> first try`, parent.ts)
    const failedWaits: Promise<unknown>[] = []
    const failedResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-failed-mention',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: failedMention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> first try`
        }
      }),
      {},
      waitUntilContext(failedWaits)
    )
    expect(failedResponse.status).toBe(200)
    await Promise.all(failedWaits)

    const retryMention = await postUserMessage(`<@${BOT_USER_ID}> retry`, parent.ts)
    const retryWaits: Promise<unknown>[] = []
    const retryResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-retry-mention',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: retryMention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> retry`
        }
      }),
      {},
      waitUntilContext(retryWaits)
    )
    expect(retryResponse.status).toBe(200)
    await Promise.all(retryWaits)

    expect(codexApi.executes).toHaveLength(2)
    const retryContextTexts = sessionMessageTexts(codexApi.appends[1]?.body.messages ?? [])
    expect(retryContextTexts).toContain('History that must not be lost.')
    expect(retryContextTexts.some(text => text.includes('first try'))).toBe(true)
    expect(retryContextTexts.some(text => text.includes('retry'))).toBe(true)
  })

  it('keeps v1 external org and trigger-bot allowlist behavior', async () => {
    const externalMention = await postUserMessage(`<@${BOT_USER_ID}> from external org`)
    const externalWaits: Promise<unknown>[] = []
    const externalResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-external-denied',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: 'TEXTERNAL',
          user_team: 'TEXTERNAL',
          ts: externalMention.ts,
          text: `<@${BOT_USER_ID}> from external org`
        }
      }),
      {},
      waitUntilContext(externalWaits)
    )
    expect(externalResponse.status).toBe(200)
    await Promise.all(externalWaits)
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)

    bot = createTestBot({ allowedExternalTeamIds: ['TEXTERNAL'] })
    const allowedExternalMention = await postUserMessage(`<@${BOT_USER_ID}> allowed external org`)
    const allowedExternalWaits: Promise<unknown>[] = []
    const allowedExternalResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-external-allowed',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: 'TEXTERNAL',
          user_team: 'TEXTERNAL',
          ts: allowedExternalMention.ts,
          text: `<@${BOT_USER_ID}> allowed external org`
        }
      }),
      {},
      waitUntilContext(allowedExternalWaits)
    )
    expect(allowedExternalResponse.status).toBe(200)
    await Promise.all(allowedExternalWaits)
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)

    bot = createTestBot()
    codexApi.reset()
    const botMention = await postUserMessage(`<@${BOT_USER_ID}> from another bot`)
    const botWaits: Promise<unknown>[] = []
    const botResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-bot-denied',
        event: {
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
        }
      }),
      {},
      waitUntilContext(botWaits)
    )
    expect(botResponse.status).toBe(200)
    await Promise.all(botWaits)
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)

    bot = createTestBot({ triggerBotAllowlist: ['app:AOTHERBOT'] })
    const allowedBotMention = await postUserMessage(`<@${BOT_USER_ID}> from allowed bot`)
    const allowedBotWaits: Promise<unknown>[] = []
    const allowedBotResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-bot-allowed',
        event: {
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
        }
      }),
      {},
      waitUntilContext(allowedBotWaits)
    )
    expect(allowedBotResponse.status).toBe(200)
    await Promise.all(allowedBotWaits)
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)
  })
})

function createTestBot(
  overrides: Partial<Parameters<typeof createSlackbotV2>[0]> = {}
): SlackbotV2 {
  return createSlackbotV2({
    apiKey: 'slackbotv2-api-key',
    apiUrl: codexApi.url,
    botToken: BOT_TOKEN,
    botUserId: BOT_USER_ID,
    signingSecret: SIGNING_SECRET,
    slackApiUrl,
    state: createMemoryState(),
    ...overrides
  })
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
    JSON.stringify({ type: 'turn.completed', turn: { id: 'turn-1', items: [] } })
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

async function postUserMessage(text: string, threadTs?: string): Promise<{ ts: string }> {
  const response = await slack.chat.postMessage({ channel: CHANNEL_ID, text, thread_ts: threadTs })
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
  return {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-slack-request-timestamp': String(timestamp),
      'x-slack-signature': `v0=${signature}`
    },
    body
  }
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
  threadKey: string
}

type MockSessionEvent = {
  data: string
  event: string
  id: number
  threadKey: string
}

type MockSessionApi = {
  appends: MockSessionRequest<SlackbotV2AppendMessagesRequest>[]
  autoRespond: boolean
  close(): Promise<void>
  closeStreams(): void
  creates: MockSessionRequest<SlackbotV2CreateSessionRequest>[]
  emitOutputLine(threadKey: string, line: string): void
  emitOutputLines(threadKey: string, lines: string[]): void
  eventRequests: MockSessionEventRequest[]
  executes: MockSessionRequest<SlackbotV2ExecuteSessionRequest>[]
  failNextExecute: boolean
  holdNextExecute(): () => void
  reset(): void
  streamCount: number
  url: string
}

async function startMockCodexApi(): Promise<MockSessionApi> {
  const appends: MockSessionRequest<SlackbotV2AppendMessagesRequest>[] = []
  const creates: MockSessionRequest<SlackbotV2CreateSessionRequest>[] = []
  const eventRequests: MockSessionEventRequest[] = []
  const events: MockSessionEvent[] = []
  const executes: MockSessionRequest<SlackbotV2ExecuteSessionRequest>[] = []
  const streams = new Set<ServerResponse>()
  let autoRespond = true
  let executeHold: Promise<void> | null = null
  let executeHoldRelease: (() => void) | null = null
  let eventId = 0
  let failNextExecute = false
  const port = await availablePort(4063)
  const closeStreams = () => {
    for (const stream of streams) stream.end()
    streams.clear()
  }
  const server = createServer((req, res) => {
    void handleMockCodexRequest(req, res, {
      appends,
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
      nextEventId() {
        eventId += 1
        return eventId
      },
      port,
      setFailNextExecute(value) {
        failNextExecute = value
      },
      streams
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
      creates.length = 0
      eventRequests.length = 0
      events.length = 0
      executes.length = 0
      executeHoldRelease?.()
      executeHold = null
      executeHoldRelease = null
      closeStreams()
      autoRespond = true
      eventId = 0
      failNextExecute = false
    },
    url: `http://127.0.0.1:${port}`,
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
    emitOutputLine(threadKey: string, line: string) {
      emitMockSessionEvent({
        data: line,
        event: 'session.output.line',
        events,
        id: ++eventId,
        streams,
        threadKey
      })
    },
    emitOutputLines(threadKey: string, lines: string[]) {
      for (const line of lines) api.emitOutputLine(threadKey, line)
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
    creates: MockSessionRequest<SlackbotV2CreateSessionRequest>[]
    events: MockSessionEvent[]
    eventRequests: MockSessionEventRequest[]
    executeHold: Promise<void> | null
    executes: MockSessionRequest<SlackbotV2ExecuteSessionRequest>[]
    failNextExecute: boolean
    nextEventId(): number
    port: number
    setFailNextExecute(value: boolean): void
    streams: Set<ServerResponse>
  }
): Promise<void> {
  const url = new URL(req.url ?? '/', `http://127.0.0.1:${input.port}`)
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
    await sendWebResponse(
      res,
      Response.json({
        thread_key: threadKey,
        sandbox_id: null,
        harness_type: body.harness_type,
        harness_thread_id: null,
        status: 'active'
      })
    )
    return
  }

  if (endpoint === 'events') {
    const afterEventId = Number.parseInt(url.searchParams.get('after_event_id') ?? '0', 10) || 0
    input.eventRequests.push({ threadKey, afterEventId })
    res.writeHead(200, {
      'cache-control': 'no-cache',
      connection: 'keep-alive',
      'content-type': 'text/event-stream'
    })
    input.streams.add(res)
    for (const event of input.events) {
      if (event.threadKey === threadKey && event.id > afterEventId) writeMockSseEvent(res, event)
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
  if (input.autoRespond) {
    for (const line of sampleCodexOutputLines(`Executed request ${input.executes.length}.`)) {
      emitMockSessionEvent({
        data: line,
        event: 'session.output.line',
        events: input.events,
        id: input.nextEventId(),
        streams: input.streams,
        threadKey
      })
    }
  }
  await sendWebResponse(
    res,
    Response.json({
      ok: true,
      execution_id: `exe-${input.executes.length}`,
      thread_key: threadKey,
      status: 'completed'
    })
  )
}

function emitMockSessionEvent(input: {
  data: string
  event: string
  events: MockSessionEvent[]
  id: number
  streams: Set<ServerResponse>
  threadKey: string
}): void {
  const event: MockSessionEvent = {
    data: input.data,
    event: input.event,
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
  calls: StreamCall[]
  close(): Promise<void>
  reset(): void
  url: string
}

type StreamCall = {
  body: Record<string, unknown>
  method:
    | 'assistant.threads.setStatus'
    | 'assistant.threads.setTitle'
    | 'chat.startStream'
    | 'chat.appendStream'
    | 'chat.stopStream'
  streamTs?: string
}

type StreamRecord = {
  channel: string
  text: string
  ts: string
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
  const streams = new Map<string, StreamRecord>()
  const port = await availablePort(4053)
  const server = createServer((req, res) => {
    void handlePatchedSlackRequest(req, res, {
      calls,
      port,
      streams,
      upstreamUrl
    }).catch(error => {
      res.writeHead(500, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ ok: false, error: String(error) }))
    })
  })
  await listen(server, port)
  return {
    calls,
    url: `http://127.0.0.1:${port}`,
    reset() {
      calls.length = 0
      streams.clear()
    },
    close: () => closeServer(server)
  }
}

async function handlePatchedSlackRequest(
  req: IncomingMessage,
  res: ServerResponse,
  input: {
    calls: StreamCall[]
    port: number
    streams: Map<string, StreamRecord>
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

  const path = normalizeApiPath(url.pathname)
  if (path === '/api/assistant.threads.setStatus') {
    const body = await requestBody(request)
    input.calls.push({ method: 'assistant.threads.setStatus', body })
    await sendWebResponse(res, Response.json({ ok: true }))
    return
  }
  if (path === '/api/assistant.threads.setTitle') {
    const body = await requestBody(request)
    input.calls.push({ method: 'assistant.threads.setTitle', body })
    await sendWebResponse(res, Response.json({ ok: true }))
    return
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
      await appendStream(input.upstreamUrl, request, input.streams, input.calls)
    )
    return
  }
  if (path === '/api/chat.stopStream') {
    await sendWebResponse(
      res,
      await stopStream(input.upstreamUrl, request, input.streams, input.calls)
    )
    return
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
  const posted = await postSlack(emulatorUrl, request, '/api/chat.postMessage', {
    channel,
    thread_ts: threadTs || undefined,
    text
  })
  if (!posted.ok) return Response.json(posted)
  const ts = stringField(posted.ts)
  calls.push({ method: 'chat.startStream', body, streamTs: ts })
  streams.set(streamKey(channel, ts), { channel, ts, text })
  return Response.json({ ok: true, channel, ts })
}

async function appendStream(
  emulatorUrl: string,
  request: Request,
  streams: Map<string, StreamRecord>,
  calls: StreamCall[]
): Promise<Response> {
  const body = await requestBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  calls.push({ method: 'chat.appendStream', body, streamTs: ts })
  const record = streams.get(streamKey(channel, ts)) ?? { channel, ts, text: '' }
  record.text += streamBodyText(body)
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
  calls: StreamCall[]
): Promise<Response> {
  const body = await requestBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  calls.push({ method: 'chat.stopStream', body, streamTs: ts })
  const key = streamKey(channel, ts)
  const record = streams.get(key) ?? { channel, ts, text: '' }
  const text = [record.text, streamBodyText(body)].filter(part => part.trim()).join('\n')
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
    expect(streamChunks(transcript.start.body.chunks)[0]).toEqual(
      expect.objectContaining({
        type: 'task_update',
        title: 'Thinking',
        status: 'in_progress'
      })
    )

    expect(transcript.appends.length).toBeGreaterThan(0)
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
    expect(transcript.stop.body.markdown_text).toBeUndefined()

    expect(markdownChunks).toEqual([{ type: 'markdown_text', text: answer }])
    expect(markdownText).toBe(answer)
    expect(markdownText).not.toContain('Implementation plan')
    expect(markdownText).not.toContain('Checking the command output')
    expect(markdownText).not.toContain('Inspecting the event stream')
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
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'thinking-commentary-1',
        title: 'Thinking',
        status: 'in_progress',
      })
    )
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'thinking-commentary-1',
        title: 'Thinking',
        status: 'complete',
        details: expect.stringContaining('Checking the command output')
      })
    )
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'reasoning-1',
        title: 'Thinking',
        status: 'in_progress',
        details: expect.stringContaining('Inspecting the event stream')
      })
    )
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'reasoning-1',
        title: 'Thinking',
        status: 'complete'
      })
    )
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'cmd-1',
        title: '1. Command execution',
        details: expect.stringContaining('pnpm test')
      })
    )
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'cmd-1',
        title: '1. Command execution',
        output: expect.stringContaining('tests passed')
      })
    )

    expect(renderedText).toContain('Implementation plan')
    expect(renderedText).toContain('Inspect App Server events')
    expect(renderedText).toContain('Stream Chat SDK chunks')
    expect(renderedText).toContain('Checking the command output')
    expect(renderedText).toContain('Inspecting the event stream')
    expect(renderedText).toContain('Command execution')
    expect(renderedText).toContain('pnpm test')
    expect(renderedText).toContain('tests passed')
    expect(renderedText.trim().endsWith(answer)).toBe(true)
  }
}

function expectSlackRenderedReply(text: string, answer: string): void {
  expect(text).toContain('Implementation plan')
  expect(text).toContain('Inspect App Server events')
  expect(text).toContain('Stream Chat SDK chunks')
  expect(text).toContain('Thinking')
  expect(text).toContain('Checking the command output')
  expect(text).toContain('Inspecting the event stream')
  expect(text).toContain('Command execution')
  expect(text).toContain('pnpm test')
  expect(text).toContain('tests passed')
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

function normalizeApiPath(path: string): string {
  return path.startsWith('/api/') ? path : `/api${path}`
}

function streamKey(channel: string, ts: string): string {
  return `${channel}:${ts}`
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : ''
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

async function waitFor(predicate: () => boolean, timeoutMs = 1000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return
    await new Promise(resolve => setTimeout(resolve, 10))
  }
  throw new Error('Timed out waiting for condition')
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
