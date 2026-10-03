// A scripted stand-in for the model providers. CoreDNS sends api.openai.com
// and api.anthropic.com to this server, so real harnesses reach it through
// iron-proxy exactly as they would reach the providers. Tests register
// replies keyed by a token they put in their Slack message, and read back
// every request the harness made.
//
// Provider traffic: HTTPS on 8443 (cert for the provider hostnames).
// Test control:     HTTP on 8080 (port-forwarded to the test runner).
import { randomUUID } from 'node:crypto'

type Reply = { match: string; text: string }
type Recorded = {
  at: string
  provider: 'openai' | 'anthropic'
  path: string
  match?: string
  model?: string
  /** The credential header as it arrived, after iron-proxy's substitution. */
  credential?: string
  body: unknown
}

const replies: Reply[] = []
const requests: Recorded[] = []

Bun.serve({
  port: 8080,
  async fetch(request) {
    const url = new URL(request.url)
    if (url.pathname === '/healthz') return new Response('ok')
    if (url.pathname === '/_e2e/replies' && request.method === 'POST') {
      replies.push((await request.json()) as Reply)
      return Response.json({ ok: true })
    }
    if (url.pathname === '/_e2e/requests') {
      const match = url.searchParams.get('match')
      return Response.json(match ? requests.filter(r => r.match === match) : requests)
    }
    return new Response('not found', { status: 404 })
  }
})

Bun.serve({
  port: 8443,
  tls: { cert: Bun.file('/tls/tls.crt'), key: Bun.file('/tls/tls.key') },
  async fetch(request) {
    const url = new URL(request.url)
    // Codex prefers a WebSocket transport and falls back to HTTPS when the
    // upgrade is refused; only the HTTPS transport is modeled so far.
    if (request.headers.get('upgrade')) return new Response('websocket unsupported', { status: 426 })
    if (request.method === 'POST' && url.pathname === '/v1/responses') {
      return openaiResponse(url.pathname, request.headers, await request.json())
    }
    if (request.method === 'POST' && url.pathname === '/v1/messages') {
      return anthropicMessage(url.pathname, request.headers, await request.json())
    }
    log('model_server_unmodeled_request', { host: request.headers.get('host'), path: url.pathname })
    return Response.json({ error: { message: 'not modeled by the e2e model server' } }, { status: 404 })
  }
})
log('model_server_started', {})

function openaiResponse(path: string, headers: Headers, body: any): Response {
  const reply = record('openai', path, headers.get('authorization'), body, lastUserText(body.input, 'type'))
  const text = reply?.text ?? 'ok'
  const responseId = `resp_${randomUUID()}`
  const itemId = `msg_${randomUUID()}`
  const message = (status: string, content: unknown[]) => ({
    type: 'message', id: itemId, role: 'assistant', status, content
  })
  const part = { type: 'output_text', text, annotations: [] }
  const response = (status: string, output: unknown[]) => ({
    id: responseId, object: 'response', model: body.model, status, output,
    usage: {
      input_tokens: 0, input_tokens_details: { cached_tokens: 0 },
      output_tokens: 0, output_tokens_details: { reasoning_tokens: 0 }, total_tokens: 0
    }
  })
  const events = [
    { type: 'response.created', response: response('in_progress', []) },
    { type: 'response.output_item.added', output_index: 0, item: message('in_progress', []) },
    { type: 'response.content_part.added', item_id: itemId, output_index: 0, content_index: 0, part: { ...part, text: '' } },
    { type: 'response.output_text.delta', item_id: itemId, output_index: 0, content_index: 0, delta: text },
    { type: 'response.output_text.done', item_id: itemId, output_index: 0, content_index: 0, text },
    { type: 'response.content_part.done', item_id: itemId, output_index: 0, content_index: 0, part },
    { type: 'response.output_item.done', output_index: 0, item: message('completed', [part]) },
    { type: 'response.completed', response: response('completed', [message('completed', [part])]) }
  ]
  return sse(events.map((event, sequence) => ({ ...event, sequence_number: sequence })))
}

function anthropicMessage(path: string, headers: Headers, body: any): Response {
  const reply = record('anthropic', path, headers.get('x-api-key'), body, lastUserText(body.messages, 'role'))
  const text = reply?.text ?? 'ok'
  const message = {
    id: `msg_${randomUUID()}`, type: 'message', role: 'assistant', model: body.model,
    content: [] as unknown[], stop_reason: null as string | null, stop_sequence: null,
    usage: { input_tokens: 0, output_tokens: 0 }
  }
  if (!body.stream) {
    return Response.json({ ...message, content: [{ type: 'text', text }], stop_reason: 'end_turn' })
  }
  return sse([
    { type: 'message_start', message },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 0 } },
    { type: 'message_stop' }
  ])
}

function record(
  provider: Recorded['provider'],
  path: string,
  credential: string | null,
  body: any,
  userText: string
): Reply | undefined {
  const reply = findReply(userText)
  requests.push({
    at: new Date().toISOString(),
    provider,
    path,
    match: reply?.match,
    model: body.model,
    credential: credential ?? undefined,
    body
  })
  return reply
}

function sse(events: Array<{ type: string; [field: string]: unknown }>): Response {
  const stream = events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
  return new Response(stream, { headers: { 'content-type': 'text/event-stream' } })
}

/**
 * Text of the newest user message: the turn the harness is asking about.
 * OpenAI input items are tagged by `type`, Anthropic messages only by `role`.
 */
function lastUserText(items: unknown, kind: 'type' | 'role'): string {
  if (!Array.isArray(items)) return ''
  const user = items
    .filter(item => item?.role === 'user' && (kind === 'role' || item.type === 'message'))
    .at(-1)
  if (typeof user?.content === 'string') return user.content
  if (!Array.isArray(user?.content)) return ''
  return user.content.map((part: any) => (typeof part?.text === 'string' ? part.text : '')).join('\n')
}

/**
 * The reply whose token appears last. slackbotv2 quotes earlier thread
 * messages into each turn, so the newest Slack message's token comes last.
 */
function findReply(text: string): Reply | undefined {
  let found: Reply | undefined
  let position = -1
  for (const reply of replies) {
    const index = text.lastIndexOf(reply.match)
    if (index > position) {
      found = reply
      position = index
    }
  }
  return found
}

function log(event: string, fields: Record<string, unknown>): void {
  console.log(JSON.stringify({ timestamp: new Date().toISOString(), event, ...fields }))
}
