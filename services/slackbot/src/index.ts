import { Hono, type Context, type MiddlewareHandler } from 'hono'
import { ulid } from '@std/ulid'
import { showRoutes } from 'hono/dev'
import { timeout } from 'hono/timeout'
import { requestId } from 'hono/request-id'
import { prettyJSON } from 'hono/pretty-json'
import { startFinalDeliveryPoller } from './centaur/final-delivery'
import { CentaurHandoff } from './centaur/handoff'
import { centaurApiKey, loadConfig } from './config'
import { logError, logInfo, logWarn, sanitizeLogValue } from './logging'
import {
  clientSpanOptions,
  configureOtel,
  internalSpanOptions,
  serverSpanOptions,
  spanAttributes,
  withSpan
} from './otel'
import { AgentSessionRenderer, withAgentSessionLock } from './slack/agent-session'
import { authorizeSlackOrg } from './slack/authorization'
import { CodexSessionRenderer, hasActiveCodexSession } from './slack/codex-session'
import { EventDeduper, slackDedupKey } from './slack/dedup'
import { duplicateSlackAlertText, type DuplicateSlackEventDetails } from './slack/duplicate-alert'
import { EnvSlackInstallationStore, SlackClientResolver } from './slack/installations'
import { normalizeSlackEnvelope } from './slack/normalize'
import { markdownToStreamChunks } from './slack/render'
import { verifySlackSignature } from './slack/signature'
import { shouldAckWithReaction } from './slack/trivial-ack'
import type { NormalizedSlackEvent, SlackEnvelope } from './slack/types'
import type { AnyBlock, AnyChunk } from '@slack/types'
import type { WebClient } from '@slack/web-api'

const config = loadConfig()
configureOtel()
// This is the existing deployments/runtime alert channel wired by the Helm
// chart from slackbot.runtimeErrorAlertChannel.
const deploymentAlertChannel = config.RUNTIME_ERROR_ALERT_CHANNEL.trim()
const resolver = new SlackClientResolver(
  new EnvSlackInstallationStore({
    token: config.SLACK_BOT_TOKEN,
    slackApiUrl: config.SLACK_API_URL
  }),
  { slackApiUrl: config.SLACK_API_URL }
)
const handoff = new CentaurHandoff(config)
const deduper = new EventDeduper(config.SLACK_EVENT_DEDUP_TTL_MS)
const CODEX_THREAD_RE = /\b(?:codex|agent|amp)\s+thread\b[^A-Z0-9]*(T-[A-Z0-9-]+)/i
const HARNESS_EVENT_PATH_RE = /^\/api\/slack\/agent-sessions\/[^/]+\/harness-event$/

void resolver
  .resolve({})
  .then(({ client }) => startFinalDeliveryPoller(config, client))
  .catch(error => {
    logError('final_delivery_poller_start_failed', error)
  })

type Variables = {
  slackRawBody: string
}

type WaitUntilContext = {
  waitUntil(promise: Promise<unknown>): void
}

export const app = new Hono<{ Variables: Variables }>()
  .use(prettyJSON())
  .use('*', async (c, next) => {
    if (HARNESS_EVENT_PATH_RE.test(c.req.path)) {
      await next()
      return
    }
    await withSpan(
      'centaur.slackbot.http_request',
      serverSpanOptions({
        'http.request.method': c.req.method,
        'url.path': c.req.path
      }),
      async span => {
        await next()
        spanAttributes(span, {
          'http.response.status_code': c.res.status
        })
      }
    )
  })
  .use('*', async (c, next) => {
    await next()
    logInfo('http_request', {
      method: c.req.method,
      path: c.req.path,
      status: c.res.status
    })
  })
  .use('*', timeout(5_000))
  .use(
    requestId({
      headerName: 'X-Slackbot-Request-ID',
      generator: () => ulid()
    })
  )

app
  .get('/health', c =>
    c.json({
      ok: true,
      service: 'slackbot',
      commit: process.env.COMMIT_SHA ?? 'local'
    })
  )
  .get('/health/ready', c => c.redirect('/health'))

const apiKeyMiddleware: MiddlewareHandler<{ Variables: Variables }> = async (c, next) => {
  if (!config.SLACKBOT_API_KEY) {
    return c.json({ ok: false, error: 'slackbot_api_key_not_configured' }, 503)
  }
  const authorization = c.req.header('authorization') ?? ''
  if (authorization !== `Bearer ${config.SLACKBOT_API_KEY}`) {
    return c.json({ ok: false, error: 'unauthorized' }, 401)
  }
  await next()
}

const slackSignatureMiddleware: MiddlewareHandler<{ Variables: Variables }> = async (c, next) => {
  const rawBody = await c.req.raw.text()
  const verification = verifySlackSignature({
    rawBody,
    signingSecret: config.SLACK_SIGNING_SECRET,
    signature: c.req.header('x-slack-signature') ?? null,
    timestamp: c.req.header('x-slack-request-timestamp') ?? null,
    maxAgeSeconds: config.SLACK_SIGNATURE_MAX_AGE_SECONDS
  })
  if (!verification.ok) {
    return c.json({ ok: false, error: verification.reason }, verification.status)
  }
  c.set('slackRawBody', rawBody)
  await next()
}

const slackHandler = async (c: Context<{ Variables: Variables }>) => {
  const envelope = parseSlackBody(c.get('slackRawBody'), c.req.header('content-type'))
  if (!envelope) return c.json({ ok: false, error: 'invalid_slack_payload' }, 400)
  if (envelope.type === 'url_verification') return c.json({ challenge: envelope.challenge })
  // Slack app manifests typically point interactivity at the same request URL
  // as events, so interactive payloads can arrive on any registered path.
  // Dispatch them before the event dedup logic, which assumes event envelopes.
  if (envelope.type && isSlackInteractiveType(envelope.type)) return slackActionHandler(c)

  const event = envelope.event
  const key = slackDedupKey({
    eventId: envelope.event_id,
    teamId: envelope.team_id,
    channelId: typeof event?.channel === 'string' ? event.channel : undefined,
    messageTs: typeof event?.ts === 'string' ? event.ts : undefined
  })
  if (!deduper.checkAndRemember(key)) {
    const duplicate = duplicateSlackEventDetails(envelope, event, key)
    logWarn(
      key.startsWith('message:')
        ? 'slack_duplicate_message_skipped'
        : 'slack_duplicate_event_skipped',
      {
        ...duplicate,
        alert_channel_id: deploymentAlertChannel || undefined
      }
    )
    if (deploymentAlertChannel) {
      runInBackground(c, notifyDuplicateSlackAlert(duplicate))
    }
    return c.json({ ok: true, duplicate: true })
  }

  runInBackground(c, processSlackEvent(envelope))
  return c.json({ ok: true })
}

app.post(config.CENTAUR_SLACK_EVENTS_PATH, slackSignatureMiddleware, slackHandler)
app.post('/api/slack/events', slackSignatureMiddleware, slackHandler)
app.post('/api/slack/actions', slackSignatureMiddleware, slackActionHandler)
app.post('/api/slack/options', slackSignatureMiddleware, slackHandler)
app.post('/api/slack/commands', slackSignatureMiddleware, slackCommandHandler)
app.post('/api/webhooks/slack', slackSignatureMiddleware, slackHandler)

app.post('/api/slack/messages', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    thread_ts?: string
    text: string
    blocks?: AnyBlock[]
  }>()
  const { client } = await resolver.resolve({})
  const response = await client.chat.postMessage({
    channel: body.channel,
    thread_ts: body.thread_ts,
    text: body.text,
    blocks: body.blocks
  })
  if (!response.ok) return c.json(response, 502)
  return c.json({ ok: true, channel: response.channel, ts: response.ts })
})

app.patch('/api/slack/messages', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    ts: string
    text: string
    blocks?: AnyBlock[]
  }>()
  const { client } = await resolver.resolve({})
  try {
    const response = await client.chat.update({
      channel: body.channel,
      ts: body.ts,
      text: body.text,
      blocks: body.blocks
    })
    if (!response.ok) return c.json(response, 502)
    return c.json({ ok: true, channel: response.channel, ts: response.ts })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.delete('/api/slack/messages', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    ts: string
  }>()
  const { client } = await resolver.resolve({})
  try {
    const response = await client.chat.delete({ channel: body.channel, ts: body.ts })
    if (!response.ok) return c.json(response, 502)
    return c.json({ ok: true, channel: response.channel, ts: response.ts })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.get('/api/slack/conversations/replies', apiKeyMiddleware, async c => {
  const channel = c.req.query('channel')
  const ts = c.req.query('ts')
  const limitRaw = c.req.query('limit')
  if (!channel || !ts) return c.json({ ok: false, error: 'missing_channel_or_ts' }, 400)
  const limit = limitRaw ? Number(limitRaw) : 20
  const { client } = await resolver.resolve({})
  try {
    const response = await client.conversations.replies({
      channel,
      ts,
      limit: Number.isFinite(limit) ? limit : 20
    })
    if (!response.ok) return c.json(response, 502)
    return c.json(response)
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/streams/start', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    thread_ts: string
    markdown?: string
    chunks?: AnyChunk[]
    recipient_team_id?: string
    recipient_user_id?: string
    task_display_mode?: 'plan' | 'timeline'
  }>()
  const { client } = await resolver.resolve({})
  try {
    const response = await client.chat.startStream({
      channel: body.channel,
      thread_ts: body.thread_ts,
      chunks: body.chunks ?? markdownToStreamChunks(body.markdown ?? ' '),
      recipient_team_id: body.recipient_team_id,
      recipient_user_id: body.recipient_user_id,
      task_display_mode: body.task_display_mode
    })
    if (!response.ok) return c.json(response, 502)
    return c.json({ ok: true, channel: response.channel, ts: response.ts })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/streams/append', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    ts: string
    markdown?: string
    chunks?: AnyChunk[]
  }>()
  const { client } = await resolver.resolve({})
  try {
    const response = await client.chat.appendStream({
      channel: body.channel,
      ts: body.ts,
      chunks: body.chunks ?? markdownToStreamChunks(body.markdown ?? ' ')
    })
    if (!response.ok) return c.json(response, 502)
    return c.json({ ok: true, channel: response.channel, ts: response.ts })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/streams/stop', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    ts: string
    markdown?: string
    chunks?: AnyChunk[]
    blocks?: AnyBlock[]
  }>()
  const { client } = await resolver.resolve({})
  try {
    const response = await client.chat.stopStream({
      channel: body.channel,
      ts: body.ts,
      chunks: body.chunks ?? (body.markdown ? markdownToStreamChunks(body.markdown) : undefined),
      blocks: body.blocks
    })
    if (!response.ok) return c.json(response, 502)
    return c.json({ ok: true, channel: response.channel, ts: response.ts })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/agent-sessions', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel: string
    parent_ts: string
    recipient_team_id: string
    recipient_user_id: string
    title?: string
    header?: string
  }>()
  const { client } = await resolver.resolve({})
  try {
    const result = await new AgentSessionRenderer(client).open({
      channel: body.channel,
      parentTs: body.parent_ts,
      recipientTeamId: body.recipient_team_id,
      recipientUserId: body.recipient_user_id,
      title: body.title,
      header: body.header
    })
    return c.json({ ok: true, session_id: result.sessionId })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/agent-sessions/:session_id/text', apiKeyMiddleware, async c => {
  const body = await c.req.json<{ markdown: string }>()
  const { client } = await resolver.resolve({})
  try {
    const sessionId = c.req.param('session_id')
    await withAgentSessionLock(sessionId, () =>
      new AgentSessionRenderer(client).text(sessionId, body.markdown)
    )
    return c.json({ ok: true })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/agent-sessions/:session_id/step', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    id: string
    title: string
    status?: 'pending' | 'in_progress' | 'complete' | 'error'
    details?: string
    output?: string
  }>()
  const { client } = await resolver.resolve({})
  try {
    const sessionId = c.req.param('session_id')
    await withAgentSessionLock(sessionId, () =>
      new AgentSessionRenderer(client).step(sessionId, body)
    )
    return c.json({ ok: true })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/agent-sessions/:session_id/done', apiKeyMiddleware, async c => {
  const body = await c.req.json<{ thread_id?: string }>()
  const { client } = await resolver.resolve({})
  try {
    const sessionId = c.req.param('session_id')
    await withAgentSessionLock(sessionId, async () => {
      if (hasActiveCodexSession(sessionId)) {
        await new CodexSessionRenderer(client).done(sessionId, body.thread_id)
      } else {
        await new AgentSessionRenderer(client).done(sessionId)
      }
    })
    return c.json({ ok: true })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/agent-sessions/:session_id/harness-event', apiKeyMiddleware, async c => {
  const body = await c.req.json<{ event: unknown }>()
  const { client } = await resolver.resolve({})
  try {
    const sessionId = c.req.param('session_id')
    const result = await withAgentSessionLock(sessionId, () =>
      new CodexSessionRenderer(client).event(sessionId, body.event)
    )
    return c.json({ ok: true, ...result })
  } catch (error) {
    return slackApiErrorResponse(c, error)
  }
})

app.post('/api/slack/assistant/status', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel_id: string
    thread_ts: string
    status: string
    loading_messages?: string[]
  }>()
  const { client } = await resolver.resolve({})
  const response = await client.assistant.threads.setStatus({
    channel_id: body.channel_id,
    thread_ts: body.thread_ts,
    status: body.status,
    loading_messages: body.loading_messages
  })
  if (!response.ok) return c.json(response, 502)
  return c.json({ ok: true })
})

app.post('/api/slack/assistant/title', apiKeyMiddleware, async c => {
  const body = await c.req.json<{
    channel_id: string
    thread_ts: string
    title: string
  }>()
  const { client } = await resolver.resolve({})
  const response = await client.assistant.threads.setTitle({
    channel_id: body.channel_id,
    thread_ts: body.thread_ts,
    title: body.title
  })
  if (!response.ok) return c.json(response, 502)
  return c.json({ ok: true })
})

if (process.env.NODE_ENV === 'development') showRoutes(app)

export default {
  port: config.PORT,
  fetch: app.fetch
}

function duplicateSlackEventDetails(
  envelope: SlackEnvelope,
  event: Record<string, unknown> | undefined,
  dedupeKey: string
): DuplicateSlackEventDetails {
  const messageTs = typeof event?.ts === 'string' ? event.ts : undefined
  return {
    dedupe_key: dedupeKey,
    event_id: envelope.event_id,
    team_id: envelope.team_id,
    channel_id: typeof event?.channel === 'string' ? event.channel : undefined,
    message_ts: messageTs,
    thread_ts: typeof event?.thread_ts === 'string' ? event.thread_ts : messageTs,
    event_type: typeof event?.type === 'string' ? event.type : undefined,
    codex_thread_id: codexThreadIdFromSlackEvent(event)
  }
}

async function notifyDuplicateSlackAlert(details: DuplicateSlackEventDetails): Promise<void> {
  if (!deploymentAlertChannel) return
  try {
    const { client } = await resolver.resolve({ teamId: details.team_id })
    await client.chat.postMessage({
      channel: deploymentAlertChannel,
      text: duplicateSlackAlertText(details)
    })
    logWarn('slack_duplicate_alert_posted', {
      ...details,
      alert_channel_id: deploymentAlertChannel,
      alert_posted: true
    })
  } catch (error) {
    logWarn('slack_duplicate_alert_failed', {
      ...details,
      alert_channel_id: deploymentAlertChannel,
      alert_posted: false,
      error: error instanceof Error ? error.message : String(error)
    })
  }
}

function codexThreadIdFromSlackEvent(
  event: Record<string, unknown> | undefined
): string | undefined {
  if (!event) return undefined
  for (const key of ['codex_thread_id', 'agent_thread_id', 'thread_id', 'session_id']) {
    const value = event[key]
    if (typeof value === 'string' && value.trim()) return value.trim()
  }
  return codexThreadIdFromUnknown(event)
}

function codexThreadIdFromUnknown(value: unknown): string | undefined {
  if (typeof value === 'string') return CODEX_THREAD_RE.exec(value)?.[1]
  if (!value || typeof value !== 'object') return undefined
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = codexThreadIdFromUnknown(item)
      if (found) return found
    }
    return undefined
  }
  for (const item of Object.values(value)) {
    const found = codexThreadIdFromUnknown(item)
    if (found) return found
  }
  return undefined
}

async function processSlackEvent(envelope: SlackEnvelope): Promise<void> {
  const rawEvent = envelope.event ?? {}
  await withSpan(
    'centaur.slackbot.event',
    internalSpanOptions({
      'slack.event_id': envelope.event_id,
      'slack.team_id': envelope.team_id,
      'slack.enterprise_id': envelope.enterprise_id,
      'slack.event_type': typeof rawEvent.type === 'string' ? rawEvent.type : undefined,
      'slack.channel_id': typeof rawEvent.channel === 'string' ? rawEvent.channel : undefined,
      'slack.message_ts': typeof rawEvent.ts === 'string' ? rawEvent.ts : undefined,
      'slack.thread_ts':
        typeof rawEvent.thread_ts === 'string'
          ? rawEvent.thread_ts
          : typeof rawEvent.ts === 'string'
            ? rawEvent.ts
            : undefined
    }),
    async span => {
      const authorization = authorizeSlackOrg({
        envelope,
        allowedExternalTeamIds: config.SLACKBOT_EXTERNAL_ORG_ALLOWLIST
      })
      if (!authorization.ok) {
        spanAttributes(span, {
          'centaur.slackbot.event_ignored': true,
          'centaur.slackbot.ignore_reason': 'external_org_not_allowlisted'
        })
        logWarn('slack_event_ignored_external_org_not_allowlisted', {
          external_team_id: authorization.externalTeamId,
          team_id: envelope.team_id,
          event_id: envelope.event_id
        })
        return
      }

      const { client, installation } = await resolver.resolve({
        teamId: envelope.team_id,
        enterpriseId: envelope.enterprise_id
      })
      const normalized = await normalizeSlackEnvelope({
        envelope,
        botUserId: installation.botUserId,
        botId: installation.botId,
        triggerBotAllowlist: config.SLACKBOT_TRIGGER_BOT_ALLOWLIST,
        client
      })
      if (!normalized) {
        spanAttributes(span, {
          'centaur.slackbot.event_ignored': true,
          'centaur.slackbot.ignore_reason': 'normalize_returned_null'
        })
        return
      }
      spanAttributes(span, {
        'centaur.thread_key': normalized.thread_key,
        'slack.channel_id': normalized.channel_id,
        'slack.thread_ts': normalized.thread_ts,
        'slack.user_id': normalized.user_id,
        'centaur.slackbot.is_mention': normalized.is_mention,
        'centaur.slackbot.part_count': normalized.parts.length
      })
      if (!normalized.is_mention) {
        spanAttributes(span, {
          'centaur.slackbot.event_ignored': true,
          'centaur.slackbot.ignore_reason': 'not_mention'
        })
        return
      }

      if (shouldAckWithReaction(normalized)) {
        spanAttributes(span, {
          'centaur.slackbot.event_action': 'ack_reaction'
        })
        await ackWithReaction(client, normalized)
        return
      }

      spanAttributes(span, {
        'centaur.slackbot.event_action': 'handoff'
      })
      const result = await handoff.emit(normalized)
      spanAttributes(span, {
        'centaur.slackbot.handoff_status': result.status,
        'centaur.slackbot.handoff_ok': result.ok
      })
      if (!result.ok) {
        if (result.status === 409) {
          logWarn('centaur_slack_handoff_conflict', result.body)
          return
        }
        throw new Error(`Centaur Slack handoff failed: ${result.status}`)
      }
    }
  )
}

const TRIVIAL_ACK_REACTION = 'ok_hand'

async function ackWithReaction(client: WebClient, event: NormalizedSlackEvent): Promise<void> {
  await withSpan(
    'centaur.slackbot.slack.reactions_add',
    clientSpanOptions({
      'slack.channel_id': event.channel_id,
      'slack.thread_ts': event.thread_ts,
      'centaur.thread_key': event.thread_key
    }),
    async () => {
      try {
        await client.reactions.add({
          channel: event.channel_id,
          timestamp: event.slack?.message_ts ?? event.thread_ts,
          name: TRIVIAL_ACK_REACTION
        })
      } catch (error) {
        logWarn('slack_trivial_ack_reaction_failed', {
          channel_id: event.channel_id,
          thread_ts: event.thread_ts,
          error: error instanceof Error ? error.message : String(error)
        })
      }
    }
  )
}

function slackApiErrorResponse(c: Context, error: unknown) {
  const data = (error as { data?: unknown })?.data
  if (data && typeof data === 'object') return c.json(sanitizeLogValue(data), 502)
  return c.json(
    {
      ok: false,
      error: error instanceof Error ? String(sanitizeLogValue(error.message)) : 'slack_api_error'
    },
    502
  )
}

type SlackCommandPayload = {
  command?: string
  text?: string
  user_id?: string
  user_name?: string
  channel_id?: string
  channel_name?: string
  team_id?: string
}

type SlackActionPayload = {
  type?: string
  trigger_id?: string
  user?: { id?: string }
  team?: { id?: string }
  channel?: { id?: string }
  message?: { ts?: string; thread_ts?: string }
  callback_id?: string
  actions?: Array<{ action_id?: string; value?: string }>
  view?: {
    callback_id?: string
    private_metadata?: string
    state?: {
      values?: Record<string, Record<string, { type?: string; value?: string }>>
    }
  }
}

// Block-button action_id and message-shortcut callback_id that open the
// feedback modal, and the modal callback_id handled on submission.
const FEEDBACK_OPEN_ID = 'centaur_feedback_open'
const FEEDBACK_SUBMIT_CALLBACK_ID = 'centaur_feedback_submit'

// Slack interactive payload max_length for plain_text_input is capped at 3000
// by the Block Kit API; views.open rejects anything larger.
const FEEDBACK_MESSAGE_MAX_LENGTH = 3000

function isSlackInteractiveType(type: string): boolean {
  return type === 'block_actions' || type === 'message_action' || type === 'view_submission'
}

async function slackActionHandler(c: Context<{ Variables: Variables }>) {
  const payload = parseSlackBody(
    c.get('slackRawBody'),
    c.req.header('content-type')
  ) as SlackActionPayload | null
  if (!payload?.type) return c.json({ ok: false, error: 'invalid_slack_action' }, 400)
  if (payload.type === 'block_actions') return openFeedbackModal(c, payload)
  if (payload.type === 'message_action') return openFeedbackModal(c, payload)
  if (payload.type === 'view_submission') return submitFeedbackModal(c, payload)
  return c.json({ ok: true })
}

async function openFeedbackModal(c: Context, payload: SlackActionPayload) {
  const action = payload.actions?.find(action => action.action_id === FEEDBACK_OPEN_ID)
  if (payload.type === 'block_actions' && !action) return c.json({ ok: true })
  if (payload.type === 'message_action' && payload.callback_id !== FEEDBACK_OPEN_ID) {
    return c.json({ ok: true })
  }
  if (!payload.trigger_id) return c.json({ ok: true })
  const metadata = {
    ...messageActionMetadata(payload),
    ...parseFeedbackMetadata(action?.value)
  }
  const { client } = await resolver.resolve({})
  try {
    await client.views.open({
      trigger_id: payload.trigger_id,
      view: {
        type: 'modal',
        callback_id: FEEDBACK_SUBMIT_CALLBACK_ID,
        title: { type: 'plain_text', text: 'Feedback' },
        submit: { type: 'plain_text', text: 'Send' },
        close: { type: 'plain_text', text: 'Cancel' },
        private_metadata: JSON.stringify({
          ...metadata,
          user_id: payload.user?.id,
          team_id: payload.team?.id,
          channel_id: metadata.channel ?? payload.channel?.id,
          thread_ts: metadata.thread_ts ?? payload.message?.thread_ts ?? payload.message?.ts
        }),
        blocks: [
          {
            type: 'input',
            block_id: 'feedback',
            element: {
              type: 'plain_text_input',
              action_id: 'message',
              multiline: true,
              max_length: FEEDBACK_MESSAGE_MAX_LENGTH,
              placeholder: { type: 'plain_text', text: 'What should we improve?' }
            },
            label: { type: 'plain_text', text: 'Feedback' }
          }
        ]
      }
    })
  } catch (error) {
    logError('slack_feedback_modal_open_failed', error)
    return c.json({ ok: false, error: 'feedback_modal_open_failed' }, 502)
  }
  return c.json({ ok: true })
}

async function submitFeedbackModal(c: Context, payload: SlackActionPayload) {
  if (payload.view?.callback_id !== FEEDBACK_SUBMIT_CALLBACK_ID) return c.json({ ok: true })
  const message = payload.view?.state?.values?.feedback?.message?.value?.trim() ?? ''
  if (!message) {
    return c.json({
      response_action: 'errors',
      errors: { feedback: 'Please add feedback before sending.' }
    })
  }
  const metadata = parseFeedbackMetadata(payload.view?.private_metadata)
  try {
    await postFeedbackToCentaur({
      source: metadata.source ?? 'slack_modal',
      message,
      user_id: payload.user?.id ?? metadata.user_id,
      channel_id: metadata.channel_id ?? metadata.channel,
      thread_ts: metadata.thread_ts,
      execution_id: metadata.execution_id,
      metadata: {
        slack: {
          team_id: payload.team?.id ?? metadata.team_id,
          channel_id: metadata.channel_id ?? metadata.channel,
          thread_ts: metadata.thread_ts,
          session_id: metadata.session_id,
          message_ts: metadata.message_ts,
          callback_id: metadata.callback_id
        }
      }
    })
    return c.json({ response_action: 'clear' })
  } catch (error) {
    logError('slack_feedback_store_failed', error)
    return c.json({
      response_action: 'errors',
      errors: { feedback: 'Could not save feedback. Please try again.' }
    })
  }
}

function parseFeedbackMetadata(value: string | undefined): Record<string, string> {
  if (!value) return {}
  try {
    const parsed = JSON.parse(value)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    return Object.fromEntries(
      Object.entries(parsed)
        .filter(([, entry]) => typeof entry === 'string')
        .map(([key, entry]) => [key, entry as string])
    )
  } catch {
    return {}
  }
}

function messageActionMetadata(payload: SlackActionPayload): Record<string, string> {
  if (payload.type !== 'message_action') return {}
  return Object.fromEntries(
    Object.entries({
      source: 'message_action',
      team_id: payload.team?.id,
      channel_id: payload.channel?.id,
      thread_ts: payload.message?.thread_ts ?? payload.message?.ts,
      message_ts: payload.message?.ts,
      callback_id: payload.callback_id
    }).filter(([, value]) => typeof value === 'string' && value.trim())
  ) as Record<string, string>
}

async function postFeedbackToCentaur(body: {
  source: string
  message: string
  user_id?: string
  channel_id?: string
  thread_ts?: string
  execution_id?: string
  metadata?: Record<string, unknown>
}) {
  const apiKey = centaurApiKey(config)
  const response = await fetch(new URL('/api/feedback', config.CENTAUR_API_URL), {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {})
    },
    body: JSON.stringify(body),
    // Slack expects the view_submission ack within 3 seconds.
    signal: AbortSignal.timeout(2500)
  })
  if (!response.ok) throw new Error(`Centaur feedback API returned ${response.status}`)
}

async function slackCommandHandler(c: Context<{ Variables: Variables }>) {
  const payload = parseSlackCommandBody(c.get('slackRawBody'))
  if (!payload?.command) return c.json({ ok: false, error: 'invalid_slack_command' }, 400)
  if (!config.SLACK_FEEDBACK_COMMANDS.includes(payload.command)) {
    return c.json({ response_type: 'ephemeral', text: `Unsupported command: ${payload.command}` })
  }
  if (
    config.SLACK_FEEDBACK_ALLOWED_CHANNELS.length &&
    payload.channel_id &&
    !config.SLACK_FEEDBACK_ALLOWED_CHANNELS.includes(payload.channel_id)
  ) {
    return c.json({
      response_type: 'ephemeral',
      text: 'This feedback command is not enabled in this channel.'
    })
  }
  if (!config.LINEAR_API_KEY) {
    return c.json({
      response_type: 'ephemeral',
      text: 'Linear feedback is not configured: missing LINEAR_API_KEY.'
    })
  }

  const text = (payload.text ?? '').trim()
  if (!text) {
    return c.json({
      response_type: 'ephemeral',
      text: `Usage: ${payload.command} <feedback or bug report>`
    })
  }

  try {
    const issue = await createLinearFeedbackIssue(payload, text)
    return c.json({
      response_type: 'ephemeral',
      text: `Created ${issue.identifier}: ${issue.url}`
    })
  } catch (error) {
    logError('linear_feedback_issue_create_failed', error)
    return c.json(
      {
        response_type: 'ephemeral',
        text: 'Could not create the Linear issue. The error was logged for follow-up.'
      },
      200
    )
  }
}

async function createLinearFeedbackIssue(
  payload: SlackCommandPayload,
  text: string
): Promise<{ identifier: string; url: string }> {
  const title = firstLineTitle(text)
  const description = [
    text,
    '',
    `Slack channel: ${payload.channel_name ? `#${payload.channel_name}` : payload.channel_id}`,
    `Submitted by: ${payload.user_id ? `<@${payload.user_id}>` : (payload.user_name ?? 'unknown')}`
  ].join('\n')

  const response = await fetch('https://api.linear.app/graphql', {
    method: 'POST',
    headers: {
      Authorization: config.LINEAR_API_KEY ?? '',
      'Content-Type': 'application/json'
    },
    body: JSON.stringify({
      query: `
        mutation IssueCreate($input: IssueCreateInput!) {
          issueCreate(input: $input) {
            success
            issue { identifier url }
          }
        }
      `,
      variables: {
        input: {
          title,
          description,
          teamId: config.SLACK_FEEDBACK_LINEAR_TEAM_ID,
          projectId: config.SLACK_FEEDBACK_LINEAR_PROJECT_ID
        }
      }
    })
  })

  if (!response.ok) throw new Error(`Linear API returned ${response.status}`)
  const body = (await response.json()) as {
    errors?: { message?: string }[]
    data?: { issueCreate?: { issue?: { identifier?: string; url?: string } } }
  }
  if (body.errors?.length) throw new Error(body.errors[0]?.message ?? 'Linear API error')
  const issue = body.data?.issueCreate?.issue
  if (!issue?.identifier || !issue.url) throw new Error('Linear issueCreate returned no issue')
  return { identifier: issue.identifier, url: issue.url }
}

function firstLineTitle(text: string): string {
  const line = text.split(/\r?\n/, 1)[0]?.trim() || 'Slack feedback'
  return line.length <= 120 ? line : `${line.slice(0, 117)}...`
}

function runInBackground(c: Context, promise: Promise<void>): void {
  const guarded = promise.catch((error: unknown) => {
    logError('slack_event_processing_failed', error)
  })
  const executionCtx = getExecutionContext(c)
  if (executionCtx) {
    executionCtx.waitUntil(guarded)
    return
  }
  void guarded
}

function getExecutionContext(c: Context): WaitUntilContext | null {
  try {
    return c.executionCtx
  } catch {
    return null
  }
}

function parseSlackBody(rawBody: string, contentType: string | undefined): SlackEnvelope | null {
  try {
    if (contentType?.includes('application/x-www-form-urlencoded')) {
      const form = new URLSearchParams(rawBody)
      const payload = form.get('payload')
      if (payload) return JSON.parse(payload) as SlackEnvelope
      return Object.fromEntries(form) as SlackEnvelope
    }
    return JSON.parse(rawBody) as SlackEnvelope
  } catch {
    return null
  }
}

function parseSlackCommandBody(rawBody: string): SlackCommandPayload | null {
  try {
    return Object.fromEntries(new URLSearchParams(rawBody)) as SlackCommandPayload
  } catch {
    return null
  }
}
