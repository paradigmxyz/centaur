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
  normalizeSlackText,
  type SlackbotV2,
  type SlackbotV2AppendMessagesRequest,
  type SlackbotV2ApiMessage,
  type SlackbotV2CreateSessionRequest,
  type SlackbotV2ExecuteSessionRequest,
  type SlackbotV2SessionMessage
} from '../src/index'
import { clearRequesterIdentityCacheForTests } from '../src/session-api'

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
  it('accepts Slack events on the legacy route', async () => {
    const parent = await postUserMessage('Legacy route context.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> use the legacy route`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/slack/events',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-legacy-route',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> use the legacy route`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.executes[0]?.threadKey).toBe(threadKey(parent.ts))
  })

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
    expect(firstAppend.body.messages.map(message => message.client_message_id)).toEqual([
      parent.ts,
      firstMention.ts
    ])
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
    expect(firstExecute.body.idempotency_key).toBe(firstMention.ts)
    const firstInputLine = JSON.parse(firstExecute.body.input_lines[0]!) as Record<string, unknown>
    expect(firstInputLine).toEqual(
      expect.objectContaining({
        type: 'user',
        thread_key: threadKey(parent.ts)
      })
    )
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

    const followUpAppend = codexApi.appends[1]!
    expect(followUpAppend.threadKey).toBe(threadKey(parent.ts))
    expect(followUpAppend.body.messages[0]?.client_message_id).toBe(followUp.ts)
    expect(sessionMessageTexts(followUpAppend.body.messages)).toEqual([
      'Additional detail for the subscribed thread.'
    ])

    const secondMentionAppend = codexApi.appends[2]!
    expect(sessionMessageTexts(secondMentionAppend.body.messages)[0]).toContain(
      'now execute with the latest'
    )
    const secondExecute = codexApi.executes[1]!
    expect(secondExecute.body.idempotency_key).toBe(secondMention.ts)
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
    expect(assistantStatuses[0]).toBe('Thinking...')
    expect(assistantStatuses.at(-1)).toBe('')
    expect(assistantStatuses.filter(status => status === 'Thinking...').length).toBeGreaterThanOrEqual(2)
    expect(assistantStatuses.filter(status => status === '').length).toBeGreaterThanOrEqual(2)
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
    expect(text).not.toContain('tests passed')
    expect(text).toContain('Executed request 1.')
    expect(text).toContain('Executed request 2.')

    const renderedReplies = (await threadTexts(parent.ts)).filter(reply =>
      reply.includes('Executed request')
    )
    expect(renderedReplies).toHaveLength(2)
    expectSlackRenderedReply(renderedReplies[0]!, 'Executed request 1.')
    expectSlackRenderedReply(renderedReplies[1]!, 'Executed request 2.')
  })

  it('includes all preceding Slack thread messages for a first mid-thread mention', async () => {
    const parent = await postUserMessage('Root context for the thread.')
    const firstReply = await postUserMessage('First preceding reply.', parent.ts)
    const secondReply = await postUserMessage('Second preceding reply.', parent.ts)
    const mention = await postUserMessage(`<@${BOT_USER_ID}> summarize the thread so far`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-mid-thread-history',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> summarize the thread so far`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.appends[0]!.body.messages.map(message => message.client_message_id)).toEqual([
      parent.ts,
      firstReply.ts,
      secondReply.ts,
      mention.ts
    ])
    expect(sessionMessageTexts(codexApi.appends[0]!.body.messages)).toEqual([
      'Root context for the thread.',
      'First preceding reply.',
      'Second preceding reply.',
      `@${BOT_USER_ID} summarize the thread so far`
    ])
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.executes[0]!.body.idempotency_key).toBe(mention.ts)
    const executeInput = JSON.stringify(JSON.parse(codexApi.executes[0]!.body.input_lines.at(-1)!))
    expect(executeInput).toContain('Root context for the thread.')
    expect(executeInput).toContain('First preceding reply.')
    expect(executeInput).toContain('Second preceding reply.')
    expect(executeInput).toContain('summarize the thread so far')
  })

  it('materializes Slack event files on root mentions without fetching thread replies', async () => {
    const mention = await postUserMessage(`<@${BOT_USER_ID}> inspect this root screenshot`)
    const fileUrl = `${slackApi.url}/files/captured.png`
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-root-file-mention',
        event: {
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
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> inspect the earlier screenshot`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-mid-thread-history-attachment',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> inspect the earlier screenshot`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

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

  it('injects Slack requester identity and verified GitHub handle into Codex input', async () => {
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> what is my name?`)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-requester-identity',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          text: `<@${BOT_USER_ID}> what is my name?`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

    expect(codexApi.creates[0]!.body.metadata).toEqual(
      expect.objectContaining({
        slack_user_id: USER_ID
      })
    )
    const executeMetadata = codexApi.executes[0]!.body.metadata
    expect(executeMetadata).toEqual(
      expect.objectContaining({
        github_handle: '@decofe',
        slack_display_name: 'Akshaan Kakar',
        slack_user_id: USER_ID,
        slack_user_name: 'akshaan'
      })
    )
    const input = JSON.parse(codexApi.executes[0]!.body.input_lines.at(-1)!) as {
      message: { content: Array<{ text?: string; type: string }> }
    }
    const requesterContext = contentTextWithHeading(input.message.content, '# Requester Context')
    expect(requesterContext).toContain('# Requester Context')
    expect(requesterContext).toContain(`Slack user ID: ${USER_ID}`)
    expect(requesterContext).toContain('Slack username: akshaan')
    expect(requesterContext).toContain('GitHub handle from Slack profile: @decofe')
    expect(requesterContext).toContain('Prompted by: @decofe')
    expect(input.message.content.at(-1)?.text).toBe(`@${BOT_USER_ID} what is my name?`)
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
      const mention = await postUserMessage(`<@${BOT_USER_ID}> identity cache ${index}`)
      const waits: Promise<unknown>[] = []
      const response = await bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: `Ev-slackbotv2-requester-identity-cache-${index}`,
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            text: `<@${BOT_USER_ID}> identity cache ${index}`
          }
        }),
        {},
        waitUntilContext(waits)
      )
      expect(response.status).toBe(200)
      await Promise.all(waits)
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

  it('uses the reply mention requester identity instead of the root requester', async () => {
    slackApi.setUserProfile(USER_ID, {
      name: 'alice',
      real_name: 'Alice Requester',
      fields: {
        X_GITHUB: {
          label: 'GitHub',
          value: 'alice-gh'
        }
      }
    })
    slackApi.setUserProfile(USER_B_ID, {
      name: 'bob',
      real_name: 'Bob Builder',
      fields: {
        X_GITHUB: {
          label: 'GitHub',
          value: 'https://github.com/bob-gh'
        }
      }
    })

    const rootMention = await postUserMessage(`<@${BOT_USER_ID}> start this PR thread`)
    const rootWaits: Promise<unknown>[] = []
    const rootResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-root-requester-a',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: rootMention.ts,
          text: `<@${BOT_USER_ID}> start this PR thread`
        }
      }),
      {},
      waitUntilContext(rootWaits)
    )
    expect(rootResponse.status).toBe(200)
    await Promise.all(rootWaits)

    const replyMention = await postUserMessage(
      `<@${BOT_USER_ID}> now make the PR`,
      rootMention.ts,
      slackB
    )
    const replyWaits: Promise<unknown>[] = []
    const replyResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-reply-requester-b',
        event: {
          type: 'app_mention',
          user: USER_B_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: replyMention.ts,
          thread_ts: rootMention.ts,
          text: `<@${BOT_USER_ID}> now make the PR`
        }
      }),
      {},
      waitUntilContext(replyWaits)
    )
    expect(replyResponse.status).toBe(200)
    await Promise.all(replyWaits)

    expect(codexApi.executes).toHaveLength(2)
    const rootInput = JSON.parse(codexApi.executes[0]!.body.input_lines.at(-1)!) as {
      message: { content: Array<{ text?: string; type: string }> }
    }
    const replyInput = JSON.parse(codexApi.executes[1]!.body.input_lines.at(-1)!) as {
      message: { content: Array<{ text?: string; type: string }> }
    }
    const rootContext = contentTextWithHeading(rootInput.message.content, '# Requester Context')
    const replyContext = contentTextWithHeading(replyInput.message.content, '# Requester Context')

    expect(rootContext).toContain(`Slack user ID: ${USER_ID}`)
    expect(rootContext).toContain('GitHub handle from Slack profile: @alice-gh')
    expect(replyContext).toContain(`Slack user ID: ${USER_B_ID}`)
    expect(replyContext).toContain('Slack username: bob')
    expect(replyContext).toContain('GitHub handle from Slack profile: @bob-gh')
    expect(replyContext).toContain('Prompted by: @bob-gh')
    expect(replyContext).not.toContain('@alice-gh')
  })

  it('includes reply mention requester identity when steering an active execution', async () => {
    codexApi.autoRespond = false
    slackApi.setUserProfile(USER_ID, {
      name: 'alice',
      real_name: 'Alice Requester',
      fields: {
        X_GITHUB: {
          label: 'GitHub',
          value: 'alice-gh'
        }
      }
    })
    slackApi.setUserProfile(USER_B_ID, {
      name: 'bob',
      real_name: 'Bob Builder',
      fields: {
        X_GITHUB: {
          label: 'GitHub',
          value: 'bob-gh'
        }
      }
    })

    const rootMention = await postUserMessage(`<@${BOT_USER_ID}> start a long PR run`)
    const rootWaits: Promise<unknown>[] = []
    const rootResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-active-root-requester-a',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: rootMention.ts,
          text: `<@${BOT_USER_ID}> start a long PR run`
        }
      }),
      {},
      waitUntilContext(rootWaits)
    )
    expect(rootResponse.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.eventRequests.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const replyMention = await postUserMessage(
      `<@${BOT_USER_ID}> actually attribute the PR to me`,
      rootMention.ts,
      slackB
    )
    const replyWaits: Promise<unknown>[] = []
    const replyResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-active-reply-requester-b',
        event: {
          type: 'app_mention',
          user: USER_B_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: replyMention.ts,
          thread_ts: rootMention.ts,
          text: `<@${BOT_USER_ID}> actually attribute the PR to me`
        }
      }),
      {},
      waitUntilContext(replyWaits)
    )
    expect(replyResponse.status).toBe(200)
    await Promise.all(replyWaits)

    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.appends).toHaveLength(2)
    const steeredParts = codexApi.appends[1]!.body.messages[0]!.parts
    const steeredText = steeredParts
      .map(part => (isRecord(part) && typeof part.text === 'string' ? part.text : ''))
      .join('\n')
    expect(steeredText).toContain('# Requester Context')
    expect(steeredText).toContain(`Slack user ID: ${USER_B_ID}`)
    expect(steeredText).toContain('GitHub handle from Slack profile: @bob-gh')
    expect(steeredText).toContain('Prompted by: @bob-gh')
    expect(steeredText).not.toContain('@alice-gh')

    codexApi.closeStreams()
    await Promise.all(rootWaits)
  })

  it('refreshes Slack thread context for a reply mention after a root mention', async () => {
    const rootMention = await postUserMessage(`<@${BOT_USER_ID}> start from this root mention`)
    const rootWaits: Promise<unknown>[] = []
    const rootResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-root-before-reply-mention',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: rootMention.ts,
          text: `<@${BOT_USER_ID}> start from this root mention`
        }
      }),
      {},
      waitUntilContext(rootWaits)
    )
    expect(rootResponse.status).toBe(200)
    await Promise.all(rootWaits)

    await postUserMessage('Important reply between mentions.', rootMention.ts)
    const replyMention = await postUserMessage(
      `<@${BOT_USER_ID}> now use the full thread`,
      rootMention.ts
    )
    const replyWaits: Promise<unknown>[] = []
    const replyResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-reply-mention-after-root',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: replyMention.ts,
          thread_ts: rootMention.ts,
          text: `<@${BOT_USER_ID}> now use the full thread`
        }
      }),
      {},
      waitUntilContext(replyWaits)
    )

    expect(replyResponse.status).toBe(200)
    await Promise.all(replyWaits)

    expect(codexApi.appends).toHaveLength(2)
    expect(sessionMessageTexts(codexApi.appends[0]!.body.messages)).toEqual([
      `@${BOT_USER_ID} start from this root mention`
    ])
    expect(sessionMessageTexts(codexApi.appends[1]!.body.messages)).toEqual([
      'Important reply between mentions.',
      `@${BOT_USER_ID} now use the full thread`
    ])
    expect(codexApi.executes.map(execute => execute.body.idempotency_key)).toEqual([
      rootMention.ts,
      replyMention.ts
    ])
    const replyExecuteInput = JSON.stringify(
      JSON.parse(codexApi.executes[1]!.body.input_lines.at(-1)!)
    )
    expect(replyExecuteInput).toContain('start from this root mention')
    expect(replyExecuteInput).toContain('Important reply between mentions.')
    expect(replyExecuteInput).toContain('now use the full thread')
  })

  it('stages large Slack file attachments without exceeding session input line limits', async () => {
    const parent = await postUserMessage('Context before the video upload.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> inspect this mp4`, parent.ts)
    const fileUrl = `${slackApi.url}/files/large-upload.mp4`
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-large-mp4',
        event: {
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
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

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
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-root-mention-thread-not-found',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          text: `<@${BOT_USER_ID}> answer from a new root message`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

    expect(codexApi.creates.map(create => create.threadKey)).toEqual([threadKey(mention.ts)])
    expect(codexApi.appends).toHaveLength(1)
    expect(sessionMessageTexts(codexApi.appends[0]!.body.messages)).toEqual([
      `@${BOT_USER_ID} answer from a new root message`
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

    const mention = await postUserMessage(`<@${BOT_USER_ID}> answer after bootstrap noise`)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-bootstrap-noise',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          text: `<@${BOT_USER_ID}> answer after bootstrap noise`
        }
      }),
      {},
      waitUntilContext(waits)
    )
    expect(response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)

    const key = threadKey(mention.ts)
    codexApi.emitOutputLine(key, 'installed 62 Centaur tool CLI shims into /home/agent/.local/bin')
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Answer despite bootstrap noise.'))

    await Promise.all(waits)
    expect(slackApi.calls.some(call => call.method === 'chat.stopStream')).toBe(true)
    expect(await threadText(mention.ts)).toContain('Answer despite bootstrap noise.')
    expect(await threadText(mention.ts)).not.toContain(
      'Execution completed, but no final text was captured.'
    )
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
    const secondAppendTexts = sessionMessageTexts(codexApi.appends[1]!.body.messages)
    expect(secondAppendTexts[0]).toContain('# Requester Context')
    expect(secondAppendTexts.at(-1)).toBe(`@${BOT_USER_ID} add this while still running`)

    codexApi.closeStreams()
    await Promise.all(firstWaits)
  })

  it('renders raw turn.failed session output as visible final text', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a raw failure.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> run a failing turn`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-raw-turn-failed',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> run a failing turn`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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

  it('renders successful completions with no final answer as visible Slack text', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before an empty completion.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> complete with no final text`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-empty-completion',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> complete with no final text`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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
      execution_id: 'exe-empty',
      status: 'completed'
    })

    await Promise.all(waits)
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const markdownChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'markdown_text')
    expect(markdownChunks).toEqual([
      {
        type: 'markdown_text',
        text: 'Execution completed, but no final text was captured.'
      }
    ])
    const renderedText = transcripts[0]!.chunks.map(chunkText).filter(Boolean).join('\n')
    expect(renderedText).toContain('Command execution')
    expect(renderedText.trim().endsWith('Execution completed, but no final text was captured.')).toBe(
      true
    )
  })

  it('renders api-rs completion result text when no final answer delta streamed', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a terminal completion.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> complete from terminal payload`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-terminal-result-text',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> complete from terminal payload`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> generate noisy progress`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-msg-too-long-fallback',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> generate noisy progress`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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

  it('swaps the streamed message for the durable final answer when the live answer diverges', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()
    bot = createTestBot({ state: sharedState })
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a diverging render.')
    const mentionText = `<@${BOT_USER_ID}> answer with a late correction`
    const mention = await postUserMessage(mentionText, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-divergence-swap',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: mentionText
        }
      }),
      {},
      waitUntilContext(waits)
    )
    expect(response.status).toBe(200)
    await waitFor(() => codexApi.executes.length === 1)
    await waitFor(() => codexApi.streamCount === 1)

    const draft = 'Draft answer from the live deltas.'
    const finalAnswer = 'Final reconciled answer from the result.'
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

    await Promise.all(waits)
    await waitFor(async () => {
      const threadState = await sharedState.get<Record<string, unknown>>(`thread-state:${key}`)
      return threadState?.renderObligation === null
    }, 3000)

    const texts = await threadTexts(parent.ts)
    // The streamed message was replaced in place with the durable final answer...
    expect(texts.filter(text => text.includes(finalAnswer))).toHaveLength(1)
    // ...and the diverging live draft is gone (neither interleaved nor left behind).
    expect(texts.some(text => text.includes('Draft answer from the live deltas'))).toBe(false)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> run something long`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-stream-expired-fallback',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> run something long`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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
    codexApi.emitSessionEvent(key, 'session.execution_completed', {
      execution_id: 'exe-stream-expired',
      status: 'completed',
      result_text: 'EXPIRED_STREAM_FALLBACK_VISIBLE'
    })

    await Promise.all(waits)
    const texts = await threadTexts(parent.ts)
    const visibleFinalReplies = texts.filter(text =>
      text.includes('EXPIRED_STREAM_FALLBACK_VISIBLE')
    )
    expect(visibleFinalReplies).toHaveLength(1)
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
    try {
      codexApi.autoRespond = false

      const parent = await postUserMessage('Context before a slow render.')
      const mention = await postUserMessage(`<@${BOT_USER_ID}> work slowly`, parent.ts)
      const key = threadKey(parent.ts)
      const waits: Promise<unknown>[] = []
      const response = await bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: 'Ev-slackbotv2-age-rotation',
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            thread_ts: parent.ts,
            text: `<@${BOT_USER_ID}> work slowly`
          }
        }),
        {},
        waitUntilContext(waits)
      )

      expect(response.status).toBe(200)
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

      await Promise.all(waits)
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
    try {
      codexApi.autoRespond = false

      const parent = await postUserMessage('Context before an aged open task.')
      const mention = await postUserMessage(`<@${BOT_USER_ID}> keep thinking`, parent.ts)
      const key = threadKey(parent.ts)
      const waits: Promise<unknown>[] = []
      const response = await bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: 'Ev-slackbotv2-open-task-age-rotation',
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            thread_ts: parent.ts,
            text: `<@${BOT_USER_ID}> keep thinking`
          }
        }),
        {},
        waitUntilContext(waits)
      )

      expect(response.status).toBe(200)
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

      await Promise.all(waits)
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
      const mention = await postUserMessage(`<@${BOT_USER_ID}> run many steps`, parent.ts)
      const key = threadKey(parent.ts)
      const waits: Promise<unknown>[] = []
      const response = await bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: 'Ev-slackbotv2-budget-rotation',
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            thread_ts: parent.ts,
            text: `<@${BOT_USER_ID}> run many steps`
          }
        }),
        {},
        waitUntilContext(waits)
      )

      expect(response.status).toBe(200)
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

      await Promise.all(waits)
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
      const mention = await postUserMessage(`<@${BOT_USER_ID}> keep one step open`, parent.ts)
      const key = threadKey(parent.ts)
      const waits: Promise<unknown>[] = []
      const response = await bot.app.request(
        '/api/webhooks/slack',
        signedSlackEvent({
          event_id: 'Ev-slackbotv2-open-task-structured-spillover',
          event: {
            type: 'app_mention',
            user: USER_ID,
            channel: CHANNEL_ID,
            team: TEAM_ID,
            ts: mention.ts,
            thread_ts: parent.ts,
            text: `<@${BOT_USER_ID}> keep one step open`
          }
        }),
        {},
        waitUntilContext(waits)
      )

      expect(response.status).toBe(200)
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

      await Promise.all(waits)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> run enough steps to paginate`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-structured-payload-budget',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> run enough steps to paginate`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> write a long visible answer`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-stream-continuation',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> write a long visible answer`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts.length).toBeGreaterThan(1)
    expect(await threadText(parent.ts)).toContain('STREAM_CONTINUATION_START')
    expect(await threadText(parent.ts)).toContain('STREAM_CONTINUATION_END')
  })

  it('conflates rapid task updates instead of one Slack call per event', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a chatty command.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> run the chatty command`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-conflated-render',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> run the chatty command`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> run many small steps`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-task-stream-continuation',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> run many small steps`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> run steps with one slow command`, parent.ts)
    const key = threadKey(parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-open-task-stream-continuation',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> run steps with one slow command`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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
    const mention = await postUserMessage(`<@${BOT_USER_ID}> wait for actual output`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-no-empty-stream-before-output',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> wait for actual output`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const renderedText = transcripts[0]!.chunks.map(chunkText).filter(Boolean).join('\n')
    expect(renderedText).toContain('DELAYED_VISIBLE_OUTPUT')
  })

  it('does not duplicate final text when execution completion follows final answer deltas', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before a completion snapshot.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> guard against duplicate final text`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-no-duplicate-completion',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> guard against duplicate final text`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
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

  it('omits large structured task output so final markdown still delivers', async () => {
    codexApi.autoRespond = false

    const parent = await postUserMessage('Context before large task output.')
    const mention = await postUserMessage(`<@${BOT_USER_ID}> keep final text visible`, parent.ts)
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-large-task-output',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> keep final text visible`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
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

    await Promise.all(waits)
    const transcripts = slackStreamTranscripts(slackApi.calls)
    expect(transcripts).toHaveLength(1)
    const taskChunks = transcripts[0]!.chunks.filter(chunk => chunk.type === 'task_update')
    expect(taskChunks).not.toHaveLength(0)
    expect(taskChunks.every(chunk => stringField(chunk.output) === '')).toBe(true)
    expect(taskChunks.every(chunk => !chunkText(chunk).includes('large-context-line'))).toBe(true)
    expect(taskChunks.some(chunk => chunkText(chunk).includes('slack thread --json --page 0'))).toBe(
      true
    )
    expect(
      taskChunks
        .map(chunk => stringField(chunk.details))
        .filter(Boolean)
        .every(details => details.length <= 500)
    ).toBe(true)
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
    const mention = await postUserMessage(
      `<@${BOT_USER_ID}> Answer from context only. Plain text only, no interactive blocks or dashboards. Include id plain-text-regression.`,
      parent.ts
    )
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-plain-text-only',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: `<@${BOT_USER_ID}> Answer from context only. Plain text only, no interactive blocks or dashboards. Include id plain-text-regression.`
        }
      }),
      {},
      waitUntilContext(waits)
    )

    expect(response.status).toBe(200)
    await Promise.all(waits)

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
        retryable_error_count: 0,
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

  it('recovers unfinished render obligations from Chat SDK state on startup', async () => {
    const sharedState = createMemoryState()
    await sharedState.connect()

    const parent = await postUserMessage('Context before restart recovery.')
    const mentionText = `<@${BOT_USER_ID}> recover a completed run`
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
        executionId: 'exe-recovery',
        message
      }
    })
    await sharedState.appendToList('slackbotv2:render:index', key)
    codexApi.emitOutputLines(key, sampleCodexOutputLines('Recovered request.'))

    bot = createTestBot({ state: sharedState })

    await waitFor(() => codexApi.eventRequests.length === 1, 2000)
    await waitFor(() => slackApi.calls.some(call => call.method === 'chat.stopStream'), 2000)

    expect(codexApi.creates).toHaveLength(0)
    expect(codexApi.appends).toHaveLength(0)
    expect(codexApi.executes).toHaveLength(0)
    expect(codexApi.eventRequests).toEqual([
      { afterEventId: 0, executionId: 'exe-recovery', threadKey: key }
    ])
    expectSlackPlanStreamShape(slackApi.calls, {
      answers: ['Recovered request.'],
      parentTs: parent.ts
    })
    expect(await threadText(parent.ts)).toContain('Recovered request.')
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
    const waits: Promise<unknown>[] = []
    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-sweep-race',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: mentionText
        }
      }),
      {},
      waitUntilContext(waits)
    )
    expect(response.status).toBe(200)

    const key = threadKey(parent.ts)
    await waitFor(() => codexApi.executes.length === 1, 2000)
    const outputLines = sampleCodexOutputLines('Single answer despite the sweep.')
    // Everything except turn/completed: the live render stays in-flight...
    codexApi.emitOutputLines(key, outputLines.slice(0, -1))
    // ...long enough for several sweep passes to scan the live obligation.
    await sleep(1200)
    codexApi.emitOutputLines(key, outputLines.slice(-1))
    await Promise.all(waits)
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
    const waits: Promise<unknown>[] = []

    const response = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-stream-open-retry',
        event: {
          type: 'app_mention',
          user: USER_ID,
          channel: CHANNEL_ID,
          team: TEAM_ID,
          ts: mention.ts,
          thread_ts: parent.ts,
          text: mentionText
        }
      }),
      {},
      waitUntilContext(waits)
    )
    expect(response.status).toBe(200)

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
    await Promise.all(waits)

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

  it('returns 503 for retryable execute failure and lets Slack retry without duplicate append', async () => {
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
    const failedWaits: Promise<unknown>[] = []
    const failedResponse = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(failedWaits)
    )
    expect(failedResponse.status).toBe(503)
    await Promise.all(failedWaits)
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.eventRequests).toHaveLength(0)
    expect(slackApi.calls.some(call => call.method === 'chat.startStream')).toBe(false)

    const retryWaits: Promise<unknown>[] = []
    const retryResponse = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(retryWaits)
    )
    expect(retryResponse.status).toBe(200)
    await Promise.all(retryWaits)

    expect(codexApi.executes).toHaveLength(2)
    expect(codexApi.appends).toHaveLength(1)
    const retryContextTexts = sessionMessageTexts(codexApi.appends[0]?.body.messages ?? [])
    expect(retryContextTexts).toContain('History that must not be lost.')
    expect(retryContextTexts.some(text => text.includes('first try'))).toBe(true)
    expect(codexApi.eventRequests).toHaveLength(1)
    expect(await threadText(parent.ts)).toContain('Executed request 1.')
  })

  it('reuses an accepted execution when Slack retries after a lost execute response', async () => {
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
    const failedWaits: Promise<unknown>[] = []
    const failedResponse = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(failedWaits)
    )
    expect(failedResponse.status).toBe(503)
    await Promise.all(failedWaits)
    expect(codexApi.executes).toHaveLength(1)
    expect(codexApi.eventRequests).toHaveLength(0)
    expect(slackApi.calls.some(call => call.method === 'chat.startStream')).toBe(false)

    const retryWaits: Promise<unknown>[] = []
    const retryResponse = await bot.app.request(
      '/api/webhooks/slack',
      retryableEvent,
      {},
      waitUntilContext(retryWaits)
    )
    expect(retryResponse.status).toBe(200)
    await Promise.all(retryWaits)

    expect(codexApi.executes).toHaveLength(2)
    expect(codexApi.executes.map(execute => execute.body.idempotency_key)).toEqual([
      mention.ts,
      mention.ts
    ])
    expect(codexApi.appends).toHaveLength(1)
    expect(codexApi.eventRequests).toHaveLength(1)
    expect(await threadText(parent.ts)).toContain('Executed request 1.')
    expect(await threadText(parent.ts)).not.toContain('Executed request 2.')
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

    bot = createTestBot({ triggerBotAllowlist: ['bot:BOTHERBOT'] })
    codexApi.reset()
    const labeledBotMention = `<@${BOT_USER_ID}|centaur> from allowed bot message`
    const allowedBotChannelMessage = await postUserMessage(labeledBotMention)
    slackApi.reset()
    const allowedBotChannelWaits: Promise<unknown>[] = []
    const allowedBotChannelResponse = await bot.app.request(
      '/api/webhooks/slack',
      signedSlackEvent({
        event_id: 'Ev-slackbotv2-bot-message-allowed',
        event: {
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
        }
      }),
      {},
      waitUntilContext(allowedBotChannelWaits)
    )
    expect(allowedBotChannelResponse.status).toBe(200)
    await Promise.all(allowedBotChannelWaits)
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
  const idempotentExecutions = new Map<string, string>()
  const streams = new Set<ServerResponse>()
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
  calls: StreamCall[]
  close(): Promise<void>
  failRepliesWithThreadNotFound(channel: string, ts: string): void
  failStreamAppendsAfter(count: number, error: string): void
  failStreamStopsLongerThan(maxChars: number): void
  holdAssistantStatus(): () => void
  reset(): void
  setUserProfile(userId: string, profile: Record<string, unknown>): void
  userProfileMethodRequestCount(userId: string, method: string): number
  userProfileRequestCount(userId: string): number
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
  payloadChars: number
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
  const threadMessageFiles = new Map<string, Record<string, unknown>[]>()
  const userProfiles = new Map<string, Record<string, unknown>>()
  const userProfileRequests = new Map<string, number>()
  const threadNotFoundReplies = new Set<string>()
  let assistantStatusGate: Promise<void> | null = null
  let releaseAssistantStatusGate: (() => void) | null = null
  let maxStreamStopChars: number | null = null
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
      assistantStatusGate: () => assistantStatusGate,
      calls,
      maxStreamStopChars,
      port,
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
    calls,
    url: `http://127.0.0.1:${port}`,
    failRepliesWithThreadNotFound(channel: string, ts: string) {
      threadNotFoundReplies.add(slackReplyKey(channel, ts))
    },
    failStreamAppendsAfter(count: number, error: string) {
      appendFailure.remaining = count
      appendFailure.error = error
    },
    failStreamStopsLongerThan(maxChars: number) {
      maxStreamStopChars = maxChars
    },
    holdAssistantStatus() {
      if (assistantStatusGate) throw new Error('assistant status is already held')
      assistantStatusGate = new Promise(resolve => {
        releaseAssistantStatusGate = resolve
      })
      return releaseCurrentAssistantStatusGate
    },
    reset() {
      releaseCurrentAssistantStatusGate()
      calls.length = 0
      maxStreamStopChars = null
      appendFailure.remaining = -1
      appendFailure.error = ''
      threadNotFoundReplies.clear()
      threadMessageFiles.clear()
      streams.clear()
      userProfiles.clear()
      userProfileRequests.clear()
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
    assistantStatusGate: () => Promise<void> | null
    calls: StreamCall[]
    maxStreamStopChars: number | null
    port: number
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
  if (path === '/api/assistant.threads.setStatus') {
    const body = await requestBody(request)
    input.calls.push({ method: 'assistant.threads.setStatus', body })
    const gate = input.assistantStatusGate()
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
    const profile = input.userProfiles.get(userId) ?? {
      name: 'tester',
      real_name: 'Test User',
      fields: {}
    }
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
        input.maxStreamStopChars
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
  maxStreamStopChars: number | null
): Promise<Response> {
  const body = await requestBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  calls.push({ method: 'chat.stopStream', body, streamTs: ts })
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
    // Conflation may merge intermediate states into the final card update
    // when the consumer is behind, so only assert the terminal status per
    // card here; content presence is asserted on the aggregate text below.
    expect(progressChunks).toContainEqual(
      expect.objectContaining({
        type: 'task_update',
        id: 'thinking-commentary-1',
        title: 'Thinking',
        status: 'complete'
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
    expect(renderedText).toContain('Checking the command output')
    expect(renderedText).toContain('Inspecting the event stream')
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
  expect(text).toContain('Thinking')
  expect(text).toContain('Checking the command output')
  expect(text).toContain('Inspecting the event stream')
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

function normalizeApiPath(path: string): string {
  return path.startsWith('/api/') ? path : `/api${path}`
}

function streamKey(channel: string, ts: string): string {
  return `${channel}:${ts}`
}

function slackReplyKey(channel: string, ts: string): string {
  return `${channel}:${ts}`
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
