import { Hono, type Context, type MiddlewareHandler } from 'hono'
import { ulid } from '@std/ulid'
import { showRoutes } from 'hono/dev'
import { timeout } from 'hono/timeout'
import { requestId } from 'hono/request-id'
import { prettyJSON } from 'hono/pretty-json'
import { startFinalDeliveryPoller } from './centaur/final-delivery'
import { CentaurHandoff } from './centaur/handoff'
import { loadConfig } from './config'
import { logError, logWarn, sanitizeLogValue } from './logging'
import { AgentSessionRenderer, withAgentSessionLock } from './slack/agent-session'
import { authorizeSlackOrg } from './slack/authorization'
import { CodexSessionRenderer, codexFooter, hasActiveCodexSession } from './slack/codex-session'
import { EventDeduper, slackDedupKey } from './slack/dedup'
import { duplicateSlackAlertText, type DuplicateSlackEventDetails } from './slack/duplicate-alert'
import { EnvSlackInstallationStore, SlackClientResolver } from './slack/installations'
import { normalizeSlackEnvelope } from './slack/normalize'
import { markdownToStreamChunks } from './slack/render'
import { verifySlackSignature } from './slack/signature'
import type { SlackEnvelope } from './slack/types'
import type { AnyBlock, AnyChunk } from '@slack/types'

const config = loadConfig()
// This is the existing deployments/runtime alert channel wired by the Helm
// chart from slackbot.runtimeErrorAlertChannel.
const deploymentAlertChannel = config.RUNTIME_ERROR_ALERT_CHANNEL.trim()
const resolver = new SlackClientResolver(
  new EnvSlackInstallationStore({
    token: config.SLACK_BOT_TOKEN
  })
)
const handoff = new CentaurHandoff(config)
const deduper = new EventDeduper(config.SLACK_EVENT_DEDUP_TTL_MS)
const CODEX_THREAD_RE = /\b(?:codex|agent|amp)\s+thread\b[^A-Z0-9]*(T-[A-Z0-9-]+)/i

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
    await next()
    console.log('http_request', c.req.method, c.req.path, c.res.status)
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
app.post('/api/slack/actions', slackSignatureMiddleware, slackHandler)
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
      chunks: body.chunks ?? markdownToStreamChunks(body.markdown ?? ' '),
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
  }>()
  const { client } = await resolver.resolve({})
  try {
    const result = await new AgentSessionRenderer(client).open({
      channel: body.channel,
      parentTs: body.parent_ts,
      recipientTeamId: body.recipient_team_id,
      recipientUserId: body.recipient_user_id,
      title: body.title
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
  const body = await c.req.json<{ footer?: string; thread_id?: string }>()
  const { client } = await resolver.resolve({})
  try {
    const sessionId = c.req.param('session_id')
    await withAgentSessionLock(sessionId, async () => {
      const footer = body.footer ?? (body.thread_id ? codexFooter(body.thread_id) : undefined)
      if (hasActiveCodexSession(sessionId)) {
        await new CodexSessionRenderer(client).done(sessionId, body.thread_id)
      } else {
        await new AgentSessionRenderer(client).done(sessionId, footer)
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
  const authorization = authorizeSlackOrg({
    envelope,
    allowedExternalTeamIds: config.SLACKBOT_EXTERNAL_ORG_ALLOWLIST
  })
  if (!authorization.ok) {
    console.warn('slack_event_ignored_external_org_not_allowlisted', {
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
    client
  })
  if (!normalized) return
  if (!normalized.is_mention) return

  const result = await handoff.emit(normalized)
  if (!result.ok) {
    if (result.status === 409) {
      logWarn('centaur_slack_handoff_conflict', result.body)
      return
    }
    throw new Error(`Centaur Slack handoff failed: ${result.status}`)
  }
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
