// A small stateful stand-in for the Slack Web and Events APIs, deployed into
// the e2e cluster so slackbotv2 runs unmodified against a Slack it cannot tell
// apart for the methods a turn uses. A user message that mentions the bot is
// delivered to slackbotv2 as a signed app_mention event, the way Slack would.
// No dependencies, so it runs from a ConfigMap on the stock Bun image.
import { createHmac } from 'node:crypto'
import { BOT, CHANNEL, TEAM, USER, USER_TOKEN } from './slack-fixture'

type Message = {
  type: 'message'
  ts: string
  text: string
  user: string
  bot_id?: string
  thread_ts?: string
  blocks?: unknown[]
  streaming?: boolean
}

const port = Number(process.env.PORT ?? 443)
const botToken = requiredEnv('SLACK_BOT_TOKEN')
const signingSecret = requiredEnv('SLACK_SIGNING_SECRET')
const eventsUrl = requiredEnv('SLACK_EVENTS_URL')

const messages: Message[] = []
let lastTs = 0
let eventCount = 0

Bun.serve({
  port,
  async fetch(request) {
    const url = new URL(request.url)
    if (url.pathname === '/healthz') return new Response('ok')
    const method = url.pathname.match(/^\/api\/([\w.]+)$/)?.[1]
    if (!method) return new Response('not found', { status: 404 })
    const actor = authenticate(request, url)
    if (!actor) return Response.json({ ok: false, error: 'invalid_auth' })
    const body = { ...Object.fromEntries(url.searchParams), ...(await requestBody(request)) }
    return Response.json(handle(method, actor, body))
  }
})
log('fake_slack_started', { port, events_url: eventsUrl })

function handle(method: string, actor: 'bot' | 'user', body: Record<string, unknown>) {
  const channel = str(body.channel)
  switch (method) {
    case 'auth.test':
      return actor === 'bot'
        ? { ok: true, url: 'https://centaur-e2e.slack.com/', team: TEAM.name, team_id: TEAM.id, user: BOT.name, user_id: BOT.userId, bot_id: BOT.id }
        : { ok: true, url: 'https://centaur-e2e.slack.com/', team: TEAM.name, team_id: TEAM.id, user: USER.name, user_id: USER.id }
    case 'team.info':
      return { ok: true, team: TEAM }
    case 'users.info':
    case 'users.profile.get': {
      const user = slackUser(str(body.user) || (actor === 'bot' ? BOT.userId : USER.id))
      if (!user) return { ok: false, error: 'user_not_found' }
      return method === 'users.info' ? { ok: true, user } : { ok: true, profile: user.profile }
    }
    case 'bots.info':
      return str(body.bot) === BOT.id
        ? { ok: true, bot: { id: BOT.id, name: BOT.name, user_id: BOT.userId, app_id: BOT.appId, deleted: false } }
        : { ok: false, error: 'bot_not_found' }
    case 'conversations.info':
      return channel === CHANNEL.id
        ? { ok: true, channel: { id: CHANNEL.id, name: CHANNEL.name, is_channel: true, is_member: true, is_private: false, is_im: false } }
        : { ok: false, error: 'channel_not_found' }
    case 'users.conversations':
    case 'conversations.list':
      return {
        ok: true,
        channels: [{ id: CHANNEL.id, name: CHANNEL.name, is_channel: true, is_member: true, is_private: false }],
        response_metadata: { next_cursor: '' }
      }
    case 'conversations.history':
      return {
        ok: true,
        has_more: false,
        messages: messages.filter(m => !m.thread_ts || m.thread_ts === m.ts).reverse().map(withReplies)
      }
    case 'conversations.replies': {
      const root = messages.find(m => m.ts === str(body.ts))
      if (channel !== CHANNEL.id || !root) return { ok: false, error: 'thread_not_found' }
      const thread = messages.filter(m => m === root || m.thread_ts === root.ts)
      return { ok: true, has_more: false, messages: thread.map(withReplies) }
    }
    case 'chat.postMessage':
    case 'chat.startStream': {
      if (channel !== CHANNEL.id) return { ok: false, error: 'channel_not_found' }
      const message = post(actor, {
        text: str(body.markdown_text) || str(body.text) || chunksText(body.chunks),
        thread_ts: str(body.thread_ts) || undefined,
        blocks: Array.isArray(body.blocks) ? body.blocks : undefined,
        streaming: method === 'chat.startStream'
      })
      if (actor === 'user' && message.text.includes(`<@${BOT.userId}>`)) void deliverMention(message)
      return { ok: true, channel, ts: message.ts, message }
    }
    case 'chat.update':
    case 'chat.appendStream':
    case 'chat.stopStream': {
      const message = messages.find(m => m.ts === str(body.ts))
      if (!message) return { ok: false, error: 'message_not_found' }
      const text = str(body.markdown_text) || str(body.text) || chunksText(body.chunks)
      if (method === 'chat.update') {
        message.text = text
      } else {
        message.text += text
      }
      if (Array.isArray(body.blocks)) message.blocks = body.blocks
      if (method === 'chat.stopStream') message.streaming = false
      return { ok: true, channel, ts: message.ts }
    }
    case 'chat.delete': {
      const index = messages.findIndex(m => m.ts === str(body.ts))
      if (index >= 0) messages.splice(index, 1)
      return { ok: true, channel, ts: str(body.ts) }
    }
    default:
      // Status, reaction, and title calls are cosmetic for this stand-in.
      log('fake_slack_unmodeled_method', { method })
      return { ok: true }
  }
}

function post(actor: 'bot' | 'user', fields: Omit<Message, 'type' | 'ts' | 'user'>): Message {
  const message: Message = {
    type: 'message',
    ts: nextTs(),
    user: actor === 'bot' ? BOT.userId : USER.id,
    ...(actor === 'bot' ? { bot_id: BOT.id } : {}),
    ...fields
  }
  messages.push(message)
  return message
}

async function deliverMention(message: Message): Promise<void> {
  const eventTime = Math.floor(Date.now() / 1000)
  const body = JSON.stringify({
    type: 'event_callback',
    token: 'unused',
    team_id: TEAM.id,
    api_app_id: BOT.appId,
    event_id: `EvE2E${++eventCount}`,
    event_time: eventTime,
    authorizations: [{ team_id: TEAM.id, user_id: BOT.userId, is_bot: true }],
    event: {
      type: 'app_mention',
      user: message.user,
      team: TEAM.id,
      channel: CHANNEL.id,
      text: message.text,
      ts: message.ts,
      event_ts: message.ts,
      ...(message.thread_ts ? { thread_ts: message.thread_ts } : {})
    }
  })
  const signature = createHmac('sha256', signingSecret).update(`v0:${eventTime}:${body}`).digest('hex')
  try {
    const response = await fetch(eventsUrl, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'x-slack-request-timestamp': String(eventTime),
        'x-slack-signature': `v0=${signature}`
      },
      body
    })
    log('fake_slack_event_delivered', { ts: message.ts, status: response.status })
  } catch (error) {
    log('fake_slack_event_failed', { ts: message.ts, error: String(error) })
  }
}

function authenticate(request: Request, url: URL): 'bot' | 'user' | undefined {
  const token = request.headers.get('authorization')?.replace(/^Bearer\s+/i, '') ?? url.searchParams.get('token')
  if (token === botToken) return 'bot'
  if (token === USER_TOKEN) return 'user'
  return undefined
}

function slackUser(id: string) {
  if (id === BOT.userId) {
    return { id, team_id: TEAM.id, name: BOT.name, real_name: BOT.name, is_bot: true, profile: { real_name: BOT.name, display_name: BOT.name, bot_id: BOT.id } }
  }
  if (id === USER.id) {
    return { id, team_id: TEAM.id, name: USER.name, real_name: 'E2E User', is_bot: false, profile: { real_name: 'E2E User', display_name: USER.name, email: USER.email } }
  }
  return undefined
}

function withReplies(message: Message) {
  const replies = messages.filter(m => m.thread_ts === message.ts && m.ts !== message.ts)
  return replies.length ? { ...message, thread_ts: message.ts, reply_count: replies.length } : message
}

function nextTs(): string {
  lastTs = Math.max(lastTs + 1, Date.now() * 1000)
  return `${Math.floor(lastTs / 1_000_000)}.${String(lastTs % 1_000_000).padStart(6, '0')}`
}

async function requestBody(request: Request): Promise<Record<string, unknown>> {
  const raw = await request.text()
  if (!raw) return {}
  if (request.headers.get('content-type')?.includes('application/json')) return JSON.parse(raw)
  // The Slack SDK form-encodes requests and JSON-encodes structured fields.
  return Object.fromEntries(
    Array.from(new URLSearchParams(raw), ([key, value]) => [key, parseMaybeJson(value)])
  )
}

function parseMaybeJson(value: string): unknown {
  if (!/^\s*[[{]/.test(value)) return value
  try {
    return JSON.parse(value)
  } catch {
    return value
  }
}

function chunksText(chunks: unknown): string {
  if (!Array.isArray(chunks)) return ''
  return chunks
    .filter(chunk => chunk?.type === 'markdown_text' && typeof chunk.text === 'string')
    .map(chunk => chunk.text)
    .join('')
}

function str(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function requiredEnv(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required`)
  return value
}

function log(event: string, fields: Record<string, unknown>): void {
  console.log(JSON.stringify({ timestamp: new Date().toISOString(), event, ...fields }))
}
