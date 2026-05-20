import type { WebClient } from '@slack/web-api'
import { centaurApiKey, type AppConfig } from '../config'
import { slackReplyLimits } from '../constants'
import { logError } from '../logging'
import { renderMarkdownBlocks } from '../slack/render'
import { withLaminarSpan } from './laminar'

const CONSUMER_ID = `slackbot-${process.pid}`
const FINAL_DELIVERY_CHUNK_CHARS = slackReplyLimits.text.maxFallbackChars

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
        const errorMessage = error instanceof Error ? error.message : String(error)
        const msgTooLong = errorMessage.includes('msg_too_long')
        await centaur(
          config,
          `/agent/final-deliveries/${executionId}/failed`,
          {
            consumer_id: CONSUMER_ID,
            error: errorMessage,
            retry_after_seconds: 10,
            ...(msgTooLong ? { error_class: 'msg_too_long', non_retryable: true } : {})
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
  const text = extractText(payload)
  const textToPost = continuationText(payload, text) ?? text
  await postFollowups(client, channel, threadTs, splitFinalDeliveryText(textToPost))
}

async function postFollowups(
  client: WebClient,
  channel: string,
  threadTs: string,
  chunks: string[]
): Promise<void> {
  for (const chunk of chunks) {
    const response = await client.chat.postMessage({
      channel,
      thread_ts: threadTs,
      text: chunk,
      blocks: renderMarkdownBlocks(chunk),
      unfurl_links: false,
      unfurl_media: false
    })
    if (!response.ok) throw new Error(response.error ?? 'chat.postMessage failed')
  }
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

function continuationText(payload: any, text: string): string | null {
  const rawOffset = Number(payload?.slackbot_streamed_answer_chars)
  if (!Number.isFinite(rawOffset) || rawOffset <= 0) return null
  const offset = Math.floor(rawOffset)
  if (offset >= text.length) return null
  return text.slice(offset).trimStart()
}

function splitFinalDeliveryText(text: string): string[] {
  const trimmed = text.trim()
  if (!trimmed) return []
  const chunks: string[] = []
  let remaining = trimmed
  while (remaining.length > FINAL_DELIVERY_CHUNK_CHARS) {
    let cut = remaining.lastIndexOf('\n\n', FINAL_DELIVERY_CHUNK_CHARS)
    if (cut <= FINAL_DELIVERY_CHUNK_CHARS * 0.3) {
      cut = remaining.lastIndexOf('\n', FINAL_DELIVERY_CHUNK_CHARS)
    }
    if (cut <= FINAL_DELIVERY_CHUNK_CHARS * 0.3) {
      cut = remaining.lastIndexOf(' ', FINAL_DELIVERY_CHUNK_CHARS)
    }
    if (cut <= FINAL_DELIVERY_CHUNK_CHARS * 0.3) cut = FINAL_DELIVERY_CHUNK_CHARS
    chunks.push(remaining.slice(0, cut).trimEnd())
    remaining = remaining.slice(cut).trimStart()
  }
  if (remaining) chunks.push(remaining)
  return chunks
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
