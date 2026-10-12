// A scripted stand-in for the model providers. CoreDNS sends api.openai.com,
// chatgpt.com, and api.anthropic.com to this server, so real harnesses reach
// it through iron-proxy exactly as they would reach the providers, with API
// keys or with subscription (ChatGPT, Claude.ai) access tokens. Tests register
// replies keyed by a token they put in their Slack message, and read back
// every request the harness made.
//
// A reply can instead run a shell command first: the model answers with a
// call to the harness's shell tool, then with its text once the tool's output
// comes back. Commands can reach /_e2e/echo/<match> on the provider hosts,
// which records the X-E2E-Credential header iron-proxy injected.
//
// Provider traffic: HTTPS on 8443 (cert for the provider hostnames).
// Test control:     HTTP on 8080 (port-forwarded to the test runner).
import { randomUUID } from 'node:crypto'

/** A provider error to answer with instead of text: an HTTP error from Anthropic, a failed response from OpenAI. */
type ProviderError = { status: number; type: string; code?: string; message: string }
type Reply = { match: string; text: string; delayMs?: number; error?: ProviderError; run?: string }
/** A request a tool command made to the echo path, and the test credential it carried. */
type Echo = { at: string; match: string; credential?: string }
type Recorded = {
  at: string
  provider: 'openai' | 'anthropic'
  path: string
  match?: string
  model?: string
  /** The credential header as it arrived, after iron-proxy's substitution. */
  credential?: string
  /** The ChatGPT workspace a subscription request was sent for, after iron-proxy's injection. */
  account?: string
  /**
   * The whole conversation the model was asked to continue. Codex's WebSocket
   * transport sends only new input plus previous_response_id, so earlier
   * turns are rebuilt from the responses this server returned.
   */
  conversation: unknown[]
  body: unknown
}

/** What the provider identifies a request by: its credential and, for ChatGPT, the workspace. */
type Caller = { credential: string | null; account: string | null }

/** The one ChatGPT workspace the e2e subscription belongs to; e2e/stack.sh sets it. */
const chatgptAccountId = process.env.CHATGPT_ACCOUNT_ID
if (!chatgptAccountId) throw new Error('CHATGPT_ACCOUNT_ID is required')
// The Responses API, with an API key or through a ChatGPT subscription.
const RESPONSES_PATHS = ['/v1/responses', '/backend-api/codex/responses']

const replies: Reply[] = []
const requests: Recorded[] = []
const echoes: Echo[] = []
/** OpenAI response id -> the conversation including that response's answer. */
const responses = new Map<string, unknown[]>()

Bun.serve({
  port: 8080,
  async fetch(request) {
    const url = new URL(request.url)
    if (url.pathname === '/healthz') return new Response('ok')
    if (url.pathname === '/_e2e/replies' && request.method === 'POST') {
      replies.push((await request.json()) as Reply)
      return Response.json({ ok: true })
    }
    if (url.pathname === '/_e2e/echoes') {
      return Response.json(echoes.filter(echo => echo.match === url.searchParams.get('match')))
    }
    if (url.pathname === '/_e2e/requests') {
      const match = url.searchParams.get('match')
      return Response.json(match ? requests.filter(r => r.match === match) : requests)
    }
    return new Response('not found', { status: 404 })
  }
})

Bun.serve<Caller & { path: string }>({
  port: 8443,
  tls: { cert: Bun.file('/tls/tls.crt'), key: Bun.file('/tls/tls.key') },
  async fetch(request, server) {
    const url = new URL(request.url)
    const caller = {
      credential: request.headers.get('authorization') ?? request.headers.get('x-api-key'),
      account: request.headers.get('chatgpt-account-id')
    }
    if (url.pathname.startsWith('/_e2e/echo/')) {
      const credential = request.headers.get('x-e2e-credential') ?? undefined
      echoes.push({ at: new Date().toISOString(), match: url.pathname.slice('/_e2e/echo/'.length), credential })
      return new Response('echoed\n')
    }
    // Codex's preferred transport: one WebSocket per session, carrying
    // response.create requests and the same events the HTTP stream would.
    if (RESPONSES_PATHS.includes(url.pathname) && request.headers.get('upgrade')?.toLowerCase() === 'websocket') {
      if (server.upgrade(request, { data: { ...caller, path: url.pathname } })) return
      return new Response('websocket upgrade failed', { status: 400 })
    }
    if (request.method === 'POST' && RESPONSES_PATHS.includes(url.pathname)) {
      const { events, delayMs } = openaiEvents(url.pathname, caller, await request.json())
      await Bun.sleep(delayMs)
      return sse(events)
    }
    if (request.method === 'POST' && url.pathname === '/v1/messages') {
      return anthropicMessage(url.pathname, caller, await request.json())
    }
    // ChatGPT workspace discovery: the workspaces the subscription can use.
    // Codex refuses to run in a workspace it selected that is not listed.
    if (request.method === 'GET' && url.pathname === '/backend-api/wham/accounts/check') {
      const workspace = {
        id: chatgptAccountId,
        workspace_backend_origin: 'https://chatgpt.com',
        account_routing_override: 'NO_CONSTRAINT'
      }
      return Response.json({ accounts: [workspace], default_account_id: chatgptAccountId })
    }
    log('model_server_unmodeled_request', { host: request.headers.get('host'), path: url.pathname })
    return Response.json({ error: { message: 'not modeled by the e2e model server' } }, { status: 404 })
  },
  websocket: {
    async message(socket, raw) {
      const { type, ...body } = JSON.parse(String(raw))
      if (type !== 'response.create') {
        log('model_server_unmodeled_websocket_message', { type })
        return
      }
      const { events, delayMs } = openaiEvents(socket.data.path, socket.data, body)
      await Bun.sleep(delayMs)
      for (const event of events) socket.send(JSON.stringify(event))
    }
  }
})
log('model_server_started', {})

/** A scripted answer as OpenAI events, and how long to wait before sending them. */
function openaiEvents(
  path: string,
  caller: Caller,
  body: any
): { events: Array<{ type: string }>; delayMs: number } {
  const conversation = [...(responses.get(body.previous_response_id) ?? []), ...(body.input ?? [])]
  const reply = record('openai', path, caller, body, conversation)
  const text = reply?.text ?? 'ok'
  const responseId = `resp_${randomUUID()}`
  const itemId = `msg_${randomUUID()}`
  const toolCall = reply?.run && !hasToolOutput(body.input) ? shellCall([...(body.tools ?? []), ...declaredTools(conversation)], reply.run) : undefined
  if (toolCall) {
    const output = (status: string) => ({
      id: responseId, object: 'response', model: body.model, status, output: [toolCall],
      usage: {
        input_tokens: 0, input_tokens_details: { cached_tokens: 0 },
        output_tokens: 0, output_tokens_details: { reasoning_tokens: 0 }, total_tokens: 0
      }
    })
    responses.set(responseId, [...conversation, toolCall])
    return {
      events: [
        { type: 'response.created', response: { ...output('in_progress'), output: [] } },
        { type: 'response.output_item.added', output_index: 0, item: toolCall },
        { type: 'response.output_item.done', output_index: 0, item: toolCall },
        { type: 'response.completed', response: output('completed') }
      ].map((event, sequence) => ({ ...event, sequence_number: sequence })),
      delayMs: 0
    }
  }
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
  if (reply?.error) {
    const { code, message: errorMessage, type } = reply.error
    return {
      events: [
        { type: 'response.created', response: response('in_progress', []) },
        { type: 'response.failed', response: { ...response('failed', []), error: { code: code ?? type, message: errorMessage } } }
      ].map((event, sequence) => ({ ...event, sequence_number: sequence })),
      delayMs: reply.delayMs ?? 0
    }
  }
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
  responses.set(responseId, [...conversation, message('completed', [part])])
  return {
    events: events.map((event, sequence) => ({ ...event, sequence_number: sequence })),
    delayMs: reply?.delayMs ?? 0
  }
}

async function anthropicMessage(path: string, caller: Caller, body: any): Promise<Response> {
  const reply = record('anthropic', path, caller, body, body.messages ?? [])
  if (reply?.run) log('model_server_unmodeled_run', { provider: 'anthropic' })
  const text = reply?.text ?? 'ok'
  await Bun.sleep(reply?.delayMs ?? 0)
  if (reply?.error) {
    const { message: errorMessage, status, type } = reply.error
    return Response.json({ type: 'error', error: { type, message: errorMessage } }, { status })
  }
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
  caller: Caller,
  body: any,
  conversation: unknown[]
): Reply | undefined {
  const reply = findReply(lastUserText(conversation))
  requests.push({
    at: new Date().toISOString(),
    provider,
    path,
    match: reply?.match,
    model: body.model,
    credential: caller.credential ?? undefined,
    account: caller.account ?? undefined,
    conversation,
    body
  })
  return reply
}

/** Whether this request returns the output of a tool call the model made. */
function hasToolOutput(input: unknown): boolean {
  return Array.isArray(input) && input.some((item: any) => String(item?.type ?? '').endsWith('_call_output'))
}

/**
 * Tools declared in the conversation. Codex's WebSocket transport declares
 * them once, as an `additional_tools` input item grouped into namespaces.
 */
function declaredTools(conversation: unknown[]): any[] {
  const flatten = (tools: any[]): any[] =>
    tools.flatMap(tool => (tool?.type === 'namespace' ? flatten(tool.tools ?? []) : [tool]))
  return flatten(conversation.flatMap((item: any) => (item?.type === 'additional_tools' ? item.tools ?? [] : [])))
}

/** A call to whichever shell tool the harness offered, running `command`. */
function shellCall(tools: any[], command: string): Record<string, unknown> {
  const callId = `call_${randomUUID()}`
  const names = new Set(tools.map(tool => tool?.name))
  // Codex's code mode: JavaScript that calls the nested exec_command tool.
  if (tools.some(tool => tool?.type === 'custom' && tool.name === 'exec')) {
    return {
      type: 'custom_tool_call', id: `ctc_${randomUUID()}`, call_id: callId, status: 'completed', name: 'exec',
      input: `text(await tools.exec_command({ cmd: ${JSON.stringify(command)} }))`
    }
  }
  if (tools.some(tool => tool?.type === 'local_shell')) {
    return {
      type: 'local_shell_call', id: `lsh_${randomUUID()}`, call_id: callId, status: 'completed',
      action: { type: 'exec', command: ['bash', '-lc', command], env: {} }
    }
  }
  const [name, args] = names.has('exec_command')
    ? ['exec_command', { cmd: command }]
    : names.has('shell_command')
      ? ['shell_command', { command }]
      : ['shell', { command: ['bash', '-lc', command] }]
  if (!names.has(name)) log('model_server_no_shell_tool', { tools: [...names] })
  return {
    type: 'function_call', id: `fc_${randomUUID()}`, call_id: callId, status: 'completed',
    name, arguments: JSON.stringify(args)
  }
}

function sse(events: Array<{ type: string; [field: string]: unknown }>): Response {
  const stream = events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
  return new Response(stream, { headers: { 'content-type': 'text/event-stream' } })
}

/** Text of the newest user message: the turn the harness is asking about. */
function lastUserText(conversation: unknown[]): string {
  const user = conversation.filter((item: any) => item?.role === 'user').at(-1) as any
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
