import type { WebClient } from '@slack/web-api'
import { centaurApiKey, type AppConfig } from '../config'
import { logError } from '../logging'
import { AgentSessionRenderer } from '../slack/agent-session'
import { codexFooter } from '../slack/codex-session'
import { withLaminarSpan } from './laminar'

const CONSUMER_ID = `slackbot-${process.pid}`

export function startFinalDeliveryPoller(config: AppConfig, client: WebClient): void {
  if (!centaurApiKey(config)) return
  const tick = async () => {
    try {
      await pollFinalDeliveriesOnce(config, client)
    } catch (error) {
      logError('final_delivery_poll_failed', error)
    }
  }
  setInterval(tick, 2_000).unref?.()
  void tick()
}

export async function pollFinalDeliveriesOnce(config: AppConfig, client: WebClient): Promise<void> {
  const claimed = await centaur(config, '/agent/final-deliveries/claim', {
    consumer_id: CONSUMER_ID,
    platform: 'slack',
    limit: 5,
    lease_seconds: 60
  })
  const deliveries = Array.isArray(claimed.deliveries) ? claimed.deliveries : []
  for (const delivery of deliveries) {
    await withLaminarSpan('centaur.slackbot.final_delivery', delivery, async () => {
      const executionId = String(delivery.execution_id)
      try {
        await deliver(client, delivery)
        await centaur(
          config,
          `/agent/final-deliveries/${executionId}/delivered`,
          {
            consumer_id: CONSUMER_ID
          },
          delivery
        )
      } catch (error) {
        await centaur(
          config,
          `/agent/final-deliveries/${executionId}/failed`,
          {
            consumer_id: CONSUMER_ID,
            error: error instanceof Error ? error.message : String(error),
            retry_after_seconds: 10
          },
          delivery
        ).catch(failError => logError('final_delivery_mark_failed_failed', failError))
      }
    })
  }
}

async function deliver(client: WebClient, delivery: any): Promise<void> {
  const meta = delivery.delivery ?? {}
  const payload = delivery.final_payload ?? {}
  const target = targetFromDelivery(delivery)
  const channel = meta.channel_id ?? meta.channel ?? target.channel
  const threadTs = meta.thread_ts ?? target.threadTs
  if (!channel || !threadTs) throw new Error('missing_slack_delivery_target')
  const renderer = new AgentSessionRenderer(client)
  const { sessionId } = await renderer.open({
    channel,
    parentTs: threadTs,
    recipientTeamId: String(meta.team_id ?? delivery.team_id ?? target.teamId ?? ''),
    recipientUserId: String(meta.recipient_user_id ?? meta.user_id ?? delivery.user_id ?? ''),
    title: sessionTitle(payload)
  })
  await renderer.text(sessionId, extractText(payload))
  await renderer.done(sessionId, deliveryFooter(payload))
}

function sessionTitle(payload: any): string {
  const title = String(payload?.session_title ?? payload?.title ?? '').trim()
  return title || 'Execution steps'
}

function extractText(payload: any): string {
  const value = firstNonEmpty(
    payload?.result_text,
    payload?.result,
    payload?.text,
    payload?.final_text,
    payload?.message
  )
  if (value) return value

  const executionId = String(payload?.execution_id ?? '').trim()
  const suffix = executionId ? ` Execution: \`${executionId}\`.` : ''
  return `Execution completed, but no final text was captured.${suffix}`
}

function firstNonEmpty(...values: unknown[]): string {
  for (const value of values) {
    const text = value === undefined || value === null ? '' : String(value).trim()
    if (text) return text
  }
  return ''
}

function deliveryFooter(payload: any): string | undefined {
  const threadId = String(payload?.agent_thread_id ?? '').trim()
  return threadId ? codexFooter(threadId) : undefined
}

function targetFromDelivery(delivery: any): {
  teamId?: string
  channel?: string
  threadTs?: string
} {
  const threadKey = String(delivery.thread_key ?? '')
  const parts = threadKey.split(':')
  if (parts[0] === 'slack' && parts.length >= 4) {
    return { teamId: parts[1], channel: parts[2], threadTs: parts.slice(3).join(':') }
  }
  return {}
}

async function centaur(config: AppConfig, path: string, body: unknown, trace?: any): Promise<any> {
  const apiKey = centaurApiKey(config)
  const traceHeaders = centaurTraceHeaders(trace)
  const response = await fetch(new URL(path, config.CENTAUR_API_URL), {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...traceHeaders,
      ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {})
    },
    body: JSON.stringify(body)
  })
  const text = await response.text()
  const parsed: any = text ? JSON.parse(text) : {}
  if (!response.ok)
    throw new Error(
      parsed?.detail?.message ?? parsed?.detail ?? parsed?.error ?? response.statusText
    )
  return parsed
}

function centaurTraceHeaders(trace: any): Record<string, string> {
  const traceId = String(trace?.trace_id ?? '').trim()
  const threadKey = String(trace?.thread_key ?? '').trim()
  const traceparent = String(trace?.traceparent ?? '').trim()
  return {
    ...(traceId ? { 'X-Trace-Id': traceId } : {}),
    ...(threadKey ? { 'X-Centaur-Thread-Key': threadKey } : {}),
    ...(traceparent ? { traceparent } : {})
  }
}
