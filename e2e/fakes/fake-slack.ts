// A small stateful stand-in for the Slack Web, Events, and Interactivity APIs,
// deployed into the e2e cluster so slackbotv2 runs unmodified against a Slack
// it cannot tell apart for the methods a conversation uses. Like Slack with the
// app's message.* and app_mention subscriptions, every user message is
// delivered to slackbotv2 as a signed `message` event, plus an `app_mention`
// when it mentions the bot. No dependencies, so it runs from a ConfigMap on the
// stock Bun image.
//
// Test control, on the same port: POST /_e2e/actions clicks a block element
// the way Slack's interactivity delivery would, and GET /_e2e/ephemeral reads
// the ephemeral messages a user was shown.
import { createHmac } from 'node:crypto'
import { BOT, CHANNELS, TEAM, USERS, type SlackUser } from './slack-fixture'

type Actor = 'bot' | SlackUser

type Message = {
  type: 'message'
  channel: string
  ts: string
  text: string
  user: string
  bot_id?: string
  thread_ts?: string
  blocks?: unknown[]
  streaming?: boolean
  reactions?: Array<{ name: string; users: string[]; count: number }>
}

type Ephemeral = { channel: string; user: string; text: string; thread_ts?: string }

const port = Number(process.env.PORT ?? 443)
const botToken = requiredEnv('SLACK_BOT_TOKEN')
const signingSecret = requiredEnv('SLACK_SIGNING_SECRET')
const eventsUrl = requiredEnv('SLACK_EVENTS_URL')

const messages: Message[] = []
const ephemerals: Ephemeral[] = []
let lastTs = 0
// Slack retries an event three times (immediately, then after 1 and 5
// minutes); these delays are compressed to fit a turn's timeout.
const EVENT_RETRY_DELAYS_MS = [1_000, 5_000, 30_000]

Bun.serve({
  port,
  async fetch(request) {
    const url = new URL(request.url)
    if (url.pathname === '/healthz') return new Response('ok')
    if (url.pathname === '/_e2e/actions' && request.method === 'POST') {
      return Response.json(await clickAction((await request.json()) as ClickRequest))
    }
    if (url.pathname === '/_e2e/ephemeral') {
      const user = url.searchParams.get('user')
      return Response.json(ephemerals.filter(ephemeral => ephemeral.user === user))
    }
    const method = url.pathname.match(/^\/api\/([\w.]+)$/)?.[1]
    if (!method) return new Response('not found', { status: 404 })
    const actor = authenticate(request, url)
    if (!actor) return Response.json({ ok: false, error: 'invalid_auth' })
    const body = { ...Object.fromEntries(url.searchParams), ...(await requestBody(request)) }
    return Response.json(handle(method, actor, body))
  }
})
log('fake_slack_started', { port, events_url: eventsUrl })

function handle(method: string, actor: Actor, body: Record<string, unknown>) {
  const channel = str(body.channel)
  switch (method) {
    case 'auth.test':
      return actor === 'bot'
        ? { ok: true, url: 'https://centaur-e2e.slack.com/', team: TEAM.name, team_id: TEAM.id, user: BOT.name, user_id: BOT.userId, bot_id: BOT.id }
        : { ok: true, url: 'https://centaur-e2e.slack.com/', team: TEAM.name, team_id: actor.teamId, user: actor.name, user_id: actor.id }
    case 'team.info':
      return { ok: true, team: TEAM }
    case 'users.info':
    case 'users.profile.get': {
      const user = slackUser(str(body.user) || (actor === 'bot' ? BOT.userId : actor.id))
      if (!user) return { ok: false, error: 'user_not_found' }
      return method === 'users.info' ? { ok: true, user } : { ok: true, profile: user.profile }
    }
    case 'bots.info':
      return str(body.bot) === BOT.id
        ? { ok: true, bot: { id: BOT.id, name: BOT.name, user_id: BOT.userId, app_id: BOT.appId, deleted: false } }
        : { ok: false, error: 'bot_not_found' }
    case 'conversations.open': {
      const user = USERS.find(candidate => candidate.id === str(body.users).split(',')[0])
      return user ? { ok: true, channel: dmChannel(user) } : { ok: false, error: 'user_not_found' }
    }
    case 'conversations.info': {
      const known = conversation(channel)
      return known ? { ok: true, channel: known } : { ok: false, error: 'channel_not_found' }
    }
    case 'users.conversations':
    case 'conversations.list':
      return {
        ok: true,
        channels: CHANNELS.map(c => conversation(c.id)),
        response_metadata: { next_cursor: '' }
      }
    case 'conversations.history':
      return {
        ok: true,
        has_more: false,
        messages: messages
          .filter(m => m.channel === channel && (!m.thread_ts || m.thread_ts === m.ts))
          .reverse()
          .map(withReplies)
      }
    case 'conversations.replies': {
      const root = messages.find(m => m.channel === channel && m.ts === str(body.ts))
      if (!root) return { ok: false, error: 'thread_not_found' }
      const thread = messages.filter(m => m === root || m.thread_ts === root.ts)
      return { ok: true, has_more: false, messages: thread.map(withReplies) }
    }
    case 'chat.postMessage':
    case 'chat.startStream': {
      if (!conversation(channel)) return { ok: false, error: 'channel_not_found' }
      const message = post(actor, channel, {
        text: str(body.markdown_text) || str(body.text) || chunksText(body.chunks),
        thread_ts: str(body.thread_ts) || undefined,
        blocks: Array.isArray(body.blocks) ? body.blocks : undefined,
        streaming: method === 'chat.startStream'
      })
      if (actor !== 'bot') void deliverMessage(message, actor)
      return { ok: true, channel, ts: message.ts, message }
    }
    case 'chat.postEphemeral': {
      if (!conversation(channel)) return { ok: false, error: 'channel_not_found' }
      ephemerals.push({ channel, user: str(body.user), text: str(body.text), thread_ts: str(body.thread_ts) || undefined })
      return { ok: true, message_ts: nextTs() }
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
    case 'reactions.add':
    case 'reactions.remove': {
      const message = messages.find(m => m.channel === channel && m.ts === str(body.timestamp))
      if (!message) return { ok: false, error: 'message_not_found' }
      const userId = actor === 'bot' ? BOT.userId : actor.id
      const reactions = (message.reactions ??= [])
      const reaction = reactions.find(r => r.name === str(body.name))
      if (method === 'reactions.add') {
        if (reaction?.users.includes(userId)) return { ok: false, error: 'already_reacted' }
        if (reaction) reaction.users.push(userId)
        else reactions.push({ name: str(body.name), users: [userId], count: 0 })
      } else {
        if (!reaction?.users.includes(userId)) return { ok: false, error: 'no_reaction' }
        reaction.users = reaction.users.filter(id => id !== userId)
      }
      message.reactions = reactions
        .map(r => ({ ...r, count: r.users.length }))
        .filter(r => r.count > 0)
      return { ok: true }
    }
    case 'reactions.get': {
      const message = messages.find(m => m.channel === channel && m.ts === str(body.timestamp))
      return message
        ? { ok: true, type: 'message', channel, message: withReplies(message) }
        : { ok: false, error: 'message_not_found' }
    }
    default:
      // Status and title calls are cosmetic for this stand-in.
      log('fake_slack_unmodeled_method', { method })
      return { ok: true }
  }
}

function post(
  actor: Actor,
  channel: string,
  fields: Omit<Message, 'type' | 'channel' | 'ts' | 'user'>
): Message {
  const message: Message = {
    type: 'message',
    channel,
    ts: nextTs(),
    user: actor === 'bot' ? BOT.userId : actor.id,
    ...(actor === 'bot' ? { bot_id: BOT.id } : {}),
    ...fields
  }
  messages.push(message)
  return message
}

/** A public channel, or a DM between the bot and one user. */
function conversation(id: string) {
  const channel = CHANNELS.find(c => c.id === id)
  if (channel) return { ...channel, is_channel: true, is_member: true, is_private: false, is_im: false }
  const user = USERS.find(candidate => dmChannel(candidate).id === id)
  return user ? dmChannel(user) : undefined
}

function dmChannel(user: SlackUser) {
  return { id: `D${user.id.slice(1)}`, is_im: true, is_channel: false, is_member: true, user: user.id }
}

/** Each thread's latest delivery, so a thread's messages reach slackbotv2 in order. */
const threadDeliveries = new Map<string, Promise<void>>()

/**
 * Delivers a user's message as Slack would: a `message` event, plus an
 * `app_mention` when it mentions the bot. A thread's messages are delivered one
 * after another, which Slack's Events API permits. Messages delivered into one
 * thread at the same instant race slackbotv2's per-thread lock, and a mention
 * that loses the race is dropped, so the suite does not depend on that timing.
 */
function deliverMessage(message: Message, sender: SlackUser): Promise<void> {
  const thread = `${message.channel}:${message.thread_ts ?? message.ts}`
  const delivery = (threadDeliveries.get(thread) ?? Promise.resolve()).then(() => deliverNow(message, sender))
  threadDeliveries.set(thread, delivery)
  void delivery.then(() => {
    if (threadDeliveries.get(thread) === delivery) threadDeliveries.delete(thread)
  })
  return delivery
}

async function deliverNow(message: Message, sender: SlackUser): Promise<void> {
  const isDm = conversation(message.channel)?.is_im === true
  const event = {
    user: message.user,
    team: sender.teamId,
    // Slack Connect events name the sender's own workspace.
    ...(sender.teamId === TEAM.id ? {} : { user_team: sender.teamId, source_team: sender.teamId }),
    channel: message.channel,
    text: message.text,
    ts: message.ts,
    event_ts: message.ts,
    ...(message.thread_ts ? { thread_ts: message.thread_ts } : {})
  }
  const deliveries = [deliverEvent(`${message.ts}m`, { type: 'message', channel_type: isDm ? 'im' : 'channel', ...event })]
  if (!isDm && message.text.includes(`<@${BOT.userId}>`)) {
    deliveries.push(deliverEvent(`${message.ts}a`, { type: 'app_mention', ...event }))
  }
  await Promise.all(deliveries)
}

async function deliverEvent(id: string, event: Record<string, unknown>): Promise<void> {
  const body = JSON.stringify({
    type: 'event_callback',
    token: 'unused',
    team_id: TEAM.id,
    api_app_id: BOT.appId,
    // Unique across fake-slack restarts: slackbotv2 remembers delivered event
    // IDs in Postgres and drops a retry whose ID it has already seen.
    event_id: `EvE2E${id.replace('.', '')}`,
    event_time: Math.floor(Date.now() / 1000),
    authorizations: [{ team_id: TEAM.id, user_id: BOT.userId, is_bot: true }],
    event
  })
  // Like Slack, retry an undelivered event (no connection or no 2xx) with the
  // same event_id, so a slackbotv2 restart delays a turn instead of losing it.
  for (let attempt = 0; ; attempt++) {
    let failure: string
    try {
      const response = await sendSigned(body, 'application/json', attempt > 0
        ? { 'x-slack-retry-num': String(attempt), 'x-slack-retry-reason': 'http_error' }
        : {})
      if (response.ok) {
        log('fake_slack_event_delivered', { id, type: event.type, status: response.status, attempt })
        return
      }
      failure = `status ${response.status}`
    } catch (error) {
      failure = String(error)
    }
    const delayMs = EVENT_RETRY_DELAYS_MS[attempt]
    log('fake_slack_event_failed', { id, type: event.type, error: failure, attempt, retrying: delayMs !== undefined })
    if (delayMs === undefined) return
    await Bun.sleep(delayMs)
  }
}

type ClickRequest = {
  /** Who clicks. */
  user: string
  channel: string
  /** The message carrying the element. */
  message_ts: string
  action_id: string
  /** Overrides the element's value, as a forged click would. */
  value?: string
  /** Reuses an earlier click's action_ts, as a redelivered click would. */
  action_ts?: string
}

/**
 * Clicks a block element: posts a signed block_actions interaction the way
 * Slack's interactivity delivery does. Slack does not retry interactions, so
 * this delivers once and reports slackbotv2's response.
 */
async function clickAction(click: ClickRequest) {
  const user = USERS.find(candidate => candidate.id === click.user)
  const message = messages.find(m => m.channel === click.channel && m.ts === click.message_ts)
  if (!user || !message) return { ok: false, error: 'not_found' }
  const element = (message.blocks ?? [])
    .flatMap(block => ((block as { elements?: unknown[] }).elements ?? []) as Array<Record<string, unknown>>)
    .find(candidate => candidate.action_id === click.action_id)
  if (!element) return { ok: false, error: 'action_not_found' }
  const actionTs = click.action_ts ?? nextTs()
  const payload = {
    type: 'block_actions',
    team: { id: TEAM.id, domain: TEAM.domain },
    user: { id: user.id, username: user.name, name: user.name, team_id: user.teamId },
    api_app_id: BOT.appId,
    token: 'unused',
    trigger_id: `trigger-${actionTs}`,
    container: {
      type: 'message',
      message_ts: message.ts,
      channel_id: message.channel,
      is_ephemeral: false,
      ...(message.thread_ts ? { thread_ts: message.thread_ts } : {})
    },
    channel: { id: message.channel },
    message: withReplies(message),
    response_url: `https://hooks.slack.com/actions/${TEAM.id}/${actionTs}/e2e-response-token`,
    actions: [{
      action_id: element.action_id,
      block_id: element.block_id ?? 'e2e-block',
      type: element.type,
      text: element.text,
      value: click.value ?? element.value,
      action_ts: actionTs
    }]
  }
  const response = await sendSigned(`payload=${encodeURIComponent(JSON.stringify(payload))}`, 'application/x-www-form-urlencoded')
  log('fake_slack_action_delivered', { action_id: click.action_id, status: response.status })
  return { ok: true, status: response.status, action_ts: actionTs }
}

async function sendSigned(body: string, contentType: string, headers: Record<string, string> = {}): Promise<Response> {
  const timestamp = Math.floor(Date.now() / 1000)
  const signature = createHmac('sha256', signingSecret).update(`v0:${timestamp}:${body}`).digest('hex')
  return fetch(eventsUrl, {
    method: 'POST',
    headers: {
      'content-type': contentType,
      'x-slack-request-timestamp': String(timestamp),
      'x-slack-signature': `v0=${signature}`,
      ...headers
    },
    body
  })
}

function authenticate(request: Request, url: URL): Actor | undefined {
  const token = request.headers.get('authorization')?.replace(/^Bearer\s+/i, '') ?? url.searchParams.get('token')
  if (token === botToken) return 'bot'
  return USERS.find(user => user.token === token)
}

function slackUser(id: string) {
  if (id === BOT.userId) {
    return { id, team_id: TEAM.id, name: BOT.name, real_name: BOT.name, is_bot: true, profile: { real_name: BOT.name, display_name: BOT.name, bot_id: BOT.id } }
  }
  const user = USERS.find(candidate => candidate.id === id)
  if (!user) return undefined
  // users.profile.get with include_labels returns custom fields keyed by field id.
  const fields = user.github
    ? { XfE2EGITHUB: { label: 'GitHub', value: `https://github.com/${user.github}`, alt: '' } }
    : {}
  return {
    id,
    team_id: user.teamId,
    name: user.name,
    real_name: user.displayName,
    is_bot: false,
    is_stranger: user.teamId !== TEAM.id,
    profile: { real_name: user.displayName, display_name: user.displayName, email: user.email, fields }
  }
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
