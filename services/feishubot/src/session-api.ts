import type { NormalizedFeishuMessage } from './feishu-events.js'

export type FeishuApiOptions = {
  baseUrl: string
  apiKey?: string
  fetch?: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
  timeoutMs?: number
}

export class FeishuApiError extends Error {
  constructor(
    readonly action: string,
    readonly status: number,
    readonly retryable: boolean
  ) {
    super(`Centaur ${action} failed with HTTP ${status}`)
    this.name = 'FeishuApiError'
  }
}

export type AcceptedTask = {
  thread_key: string
  workspace_id: string
  selection_flow_id?: string
  execution_id: string
  created: boolean
}

export type SelectionView = {
  selection_flow_id: string
  workspace_id: string
  thread_key: string
  execution_id?: string | null
  kind: 'initial' | 'add'
  state: 'pending' | 'confirmed' | 'cancelled'
  version: number
  task_excerpt: string
  query: string
  cursor?: string | null
  cursor_history: string[]
  selected_repository_ids: string[]
}

export type SessionEvent = {
  id?: number
  event: string
  data: unknown
}

export type FeishuDelivery = {
  delivery_id: string
  tenant_key: string
  thread_key: string
  chat_id: string
  root_message_id: string
  source_message_id?: string | null
  message_id?: string | null
  last_event_cursor: number
  delivery_generation: number
  desired_version: number
  render_version: number
  state: string
  initiator_principal_id: string
  selection_flow_id?: string | null
  execution_id?: string | null
  publish_batch_id?: string | null
}

export type RepositoryPage = {
  repositories: Array<{
    repository_id: string
    name: string
    namespace: string
    path_with_namespace: string
    description?: string | null
    default_branch?: string | null
    archived: boolean
    last_activity_at?: string | null
  }>
  next_cursor?: string | null
}

export class FeishuSessionApi {
  readonly #baseUrl: URL
  readonly #apiKey?: string
  readonly #fetch: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
  readonly #timeoutMs: number

  constructor(options: FeishuApiOptions) {
    this.#baseUrl = new URL(ensureSlash(options.baseUrl))
    this.#apiKey = options.apiKey
    this.#fetch = options.fetch ?? fetch
    this.#timeoutMs = options.timeoutMs ?? 30_000
  }

  async acceptMessage(message: NormalizedFeishuMessage): Promise<AcceptedTask> {
    const channel = channelBody(message)
    const sessionMessage = messageBody(message)
    try {
      return await this.#json<AcceptedTask>('continue task', 'api/development/tasks/continue', {
        method: 'POST',
        body: JSON.stringify({
          channel,
          platform_event_id: message.eventId,
          platform_message_id: message.messageId,
          sender_principal_id: message.principalId,
          message: sessionMessage
        })
      })
    } catch (error) {
      if (!(error instanceof FeishuApiError) || error.status !== 404) throw error
    }
    return this.#json<AcceptedTask>('accept task', 'api/development/tasks', {
      method: 'POST',
      body: JSON.stringify({
        channel,
        platform_event_id: message.eventId,
        platform_message_id: message.messageId,
        harness_type: 'codex',
        initiator: { principal_id: message.principalId },
        message: sessionMessage,
        session_metadata: {
          source: 'feishu',
          tenant_key: message.tenantKey,
          chat_id: message.chatId,
          sender_open_id: message.senderOpenId,
          ...(message.senderUnionId ? { sender_union_id: message.senderUnionId } : {})
        }
      })
    })
  }

  async closeBinding(message: NormalizedFeishuMessage): Promise<{ ok: boolean; thread_key: string }> {
    return this.#json('start new task', 'api/development/tasks/new', {
      method: 'POST',
      body: JSON.stringify({
        channel: channelBody(message),
        requested_by_principal_id: message.principalId
      })
    })
  }

  async activeBinding(message: NormalizedFeishuMessage): Promise<{ thread_key: string; workspace_id: string }> {
    return this.#json('find active task', 'api/development/tasks/active', {
      method: 'POST',
      body: JSON.stringify({
        channel: channelBody(message),
        requested_by_principal_id: message.principalId
      })
    })
  }

  async createAddSelection(
    threadKey: string,
    principalId: string,
    sourceMessageId: string,
    idempotencyKey: string
  ) {
    return this.#json<{ selection_flow_id: string; workspace_id: string; version: number }>(
      'add projects',
      `api/development/sessions/${encodeURIComponent(threadKey)}/repositories`,
      {
        method: 'POST',
        body: JSON.stringify({
          requested_by_principal_id: principalId,
          source_message_id: sourceMessageId,
          idempotency_key: idempotencyKey
        })
      }
    )
  }

  async searchRepositories(query?: string, cursor?: string): Promise<RepositoryPage> {
    const url = new URL('api/development/repositories', this.#baseUrl)
    if (query?.trim()) url.searchParams.set('query', query.trim())
    if (cursor?.trim()) url.searchParams.set('cursor', cursor.trim())
    return this.#jsonUrl('search repositories', url, { method: 'GET' })
  }

  async getSelection(selectionFlowId: string, principalId: string): Promise<SelectionView> {
    const url = new URL(
      `api/development/selections/${encodeURIComponent(selectionFlowId)}`,
      this.#baseUrl
    )
    url.searchParams.set('requested_by_principal_id', principalId)
    return this.#jsonUrl('load selection', url, { method: 'GET' })
  }

  async updateSelection(
    selectionFlowId: string,
    expectedVersion: number,
    principalId: string,
    state: Pick<SelectionView, 'query' | 'cursor' | 'cursor_history' | 'selected_repository_ids'>
  ): Promise<SelectionView> {
    return this.#json('update selection', `api/development/selections/${encodeURIComponent(selectionFlowId)}`, {
      method: 'PUT',
      body: JSON.stringify({
        expected_version: expectedVersion,
        requested_by_principal_id: principalId,
        query: state.query,
        cursor: state.cursor ?? null,
        cursor_history: state.cursor_history,
        selected_repository_ids: state.selected_repository_ids
      })
    })
  }

  confirmSelection(selectionFlowId: string, expectedVersion: number, principalId: string, repositoryIds: string[]) {
    return this.#selectionMutation(selectionFlowId, 'confirm', expectedVersion, principalId, {
      repository_ids: repositoryIds
    })
  }

  confirmNoProject(selectionFlowId: string, expectedVersion: number, principalId: string) {
    return this.#selectionMutation(selectionFlowId, 'no-project', expectedVersion, principalId)
  }

  cancelSelection(selectionFlowId: string, expectedVersion: number, principalId: string) {
    return this.#selectionMutation(selectionFlowId, 'cancel', expectedVersion, principalId)
  }

  getChangeset(changesetId: string, principalId: string) {
    const url = new URL(
      `api/development/feishu/changesets/${encodeURIComponent(changesetId)}`,
      this.#baseUrl
    )
    url.searchParams.set('requested_by_principal_id', principalId)
    return this.#jsonUrl<Record<string, unknown>>('get changeset', url, { method: 'GET' })
  }

  approvePublication(changesetId: string, principalId: string, idempotencyKey: string) {
    return this.#json('approve publication', `api/development/feishu/changesets/${encodeURIComponent(changesetId)}/publish`, {
      method: 'POST', body: JSON.stringify({
        requested_by_principal_id: principalId,
        idempotency_key: idempotencyKey
      })
    })
  }

  retryPublication(publishBatchId: string, principalId: string, idempotencyKey: string) {
    return this.#json('retry publication', `api/development/feishu/publish-batches/${encodeURIComponent(publishBatchId)}/retry`, {
      method: 'POST', body: JSON.stringify({
        requested_by_principal_id: principalId,
        idempotency_key: idempotencyKey
      })
    })
  }

  getPublishBatch(publishBatchId: string, principalId: string) {
    const url = new URL(
      `api/development/feishu/publish-batches/${encodeURIComponent(publishBatchId)}`,
      this.#baseUrl
    )
    url.searchParams.set('requested_by_principal_id', principalId)
    return this.#jsonUrl<Record<string, unknown>>('get publication', url, { method: 'GET' })
  }

  getDelivery(threadKey: string): Promise<FeishuDelivery> {
    return this.#json(
      'get Feishu delivery',
      `api/development/feishu/deliveries/${encodeURIComponent(threadKey)}`,
      { method: 'GET' }
    )
  }

  listPendingDeliveries(): Promise<string[]> {
    return this.#json('list Feishu deliveries', 'api/development/feishu/deliveries/pending', {
      method: 'GET'
    })
  }

  claimDelivery(
    threadKey: string,
    expectedDeliveryGeneration: number,
    expectedDesiredVersion: number,
    leaseOwner: string
  ): Promise<FeishuDelivery> {
    return this.#json(
      'claim Feishu delivery',
      `api/development/feishu/deliveries/${encodeURIComponent(threadKey)}/claim`,
      {
        method: 'POST',
        body: JSON.stringify({
          expected_delivery_generation: expectedDeliveryGeneration,
          expected_desired_version: expectedDesiredVersion,
          lease_owner: leaseOwner
        })
      }
    )
  }

  recordDelivery(
    threadKey: string,
    messageId: string,
    lastEventCursor: number,
    expectedDesiredVersion: number,
    expectedDeliveryGeneration: number,
    leaseOwner: string,
    renderComplete: boolean
  ): Promise<FeishuDelivery> {
    return this.#json(
      'record Feishu delivery',
      `api/development/feishu/deliveries/${encodeURIComponent(threadKey)}`,
      {
        method: 'PUT',
        body: JSON.stringify({
          message_id: messageId,
          last_event_cursor: lastEventCursor,
          expected_desired_version: expectedDesiredVersion,
          expected_delivery_generation: expectedDeliveryGeneration,
          lease_owner: leaseOwner,
          render_complete: renderComplete
        })
      }
    )
  }

  async *streamEvents(threadKey: string, executionId: string, afterEventId = 0): AsyncIterable<SessionEvent> {
    const url = new URL(`api/session/${encodeURIComponent(threadKey)}/events`, this.#baseUrl)
    url.searchParams.set('execution_id', executionId)
    url.searchParams.set('after_event_id', String(afterEventId))
    const response = await this.#request('stream events', url, { method: 'GET' })
    if (!response.body) throw new FeishuApiError('stream events', 502, true)
    yield* parseSse(response.body)
  }

  #selectionMutation(
    selectionFlowId: string,
    action: string,
    expectedVersion: number,
    principalId: string,
    extra: Record<string, unknown> = {}
  ) {
    return this.#json(
      `${action} selection`,
      `api/development/selections/${encodeURIComponent(selectionFlowId)}/${action}`,
      {
        method: 'POST',
        body: JSON.stringify({
          expected_version: expectedVersion,
          decided_by_principal_id: principalId,
          ...extra
        })
      }
    )
  }

  #json<T = unknown>(action: string, path: string, init: RequestInit): Promise<T> {
    return this.#jsonUrl(action, new URL(path, this.#baseUrl), init)
  }

  async #jsonUrl<T>(action: string, url: URL, init: RequestInit): Promise<T> {
    const response = await this.#request(action, url, init)
    return await response.json() as T
  }

  async #request(action: string, url: URL, init: RequestInit): Promise<Response> {
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), this.#timeoutMs)
    try {
      const response = await this.#fetch(url, {
        ...init,
        headers: {
          ...(init.body ? { 'content-type': 'application/json' } : {}),
          ...(this.#apiKey ? { authorization: `Bearer ${this.#apiKey}` } : {}),
          ...init.headers
        },
        signal: controller.signal
      })
      if (!response.ok) {
        throw new FeishuApiError(action, response.status, retryableStatus(response.status))
      }
      return response
    } finally {
      clearTimeout(timer)
    }
  }
}

async function* parseSse(stream: ReadableStream<Uint8Array>): AsyncIterable<SessionEvent> {
  const reader = stream.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  let event = 'message'
  let id: number | undefined
  let data: string[] = []
  const emit = (): SessionEvent | undefined => {
    if (data.length === 0 && event === 'message' && id === undefined) return undefined
    const raw = data.join('\n')
    let parsed: unknown = raw
    try { parsed = JSON.parse(raw) } catch { /* SSE data may be a raw harness line. */ }
    const result = { ...(id === undefined ? {} : { id }), event, data: parsed }
    event = 'message'
    id = undefined
    data = []
    return result
  }
  try {
    while (true) {
      const chunk = await reader.read()
      if (chunk.done) break
      buffer += decoder.decode(chunk.value, { stream: true })
      let newline = buffer.indexOf('\n')
      while (newline >= 0) {
        const line = buffer.slice(0, newline).replace(/\r$/u, '')
        buffer = buffer.slice(newline + 1)
        if (!line) {
          const value = emit()
          if (value) yield value
        } else if (line.startsWith('event:')) event = line.slice(6).trim()
        else if (line.startsWith('id:')) {
          const value = Number.parseInt(line.slice(3).trim(), 10)
          if (Number.isFinite(value)) id = value
        } else if (line.startsWith('data:')) data.push(line.slice(5).trimStart())
        newline = buffer.indexOf('\n')
      }
    }
    buffer += decoder.decode()
    if (buffer.startsWith('data:')) data.push(buffer.slice(5).trimStart())
    const value = emit()
    if (value) yield value
  } finally {
    await reader.cancel().catch(() => undefined)
    reader.releaseLock()
  }
}

function channelBody(message: NormalizedFeishuMessage) {
  return {
    platform: 'feishu',
    tenant_key: message.tenantKey,
    conversation_key: message.conversationKey,
    root_message_id: message.rootMessageId
  }
}

function messageBody(message: NormalizedFeishuMessage) {
  return {
    client_message_id: message.messageId,
    role: 'user',
    parts: [{ type: 'text', text: message.text }],
    metadata: {
      source: 'feishu',
      event_id: message.eventId,
      message_id: message.messageId,
      sender_open_id: message.senderOpenId,
      sender_principal_id: message.principalId
    }
  }
}

function retryableStatus(status: number): boolean {
  return status === 408 || status === 425 || status === 429 || status >= 500
}

function ensureSlash(value: string): string {
  return value.endsWith('/') ? value : `${value}/`
}
