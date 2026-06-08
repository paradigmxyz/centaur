/**
 * Normalize harness-specific JSON events into canonical thread events.
 *
 * The thread UI stream mapper expects a small set of event shapes (`assistant`, `tool`,
 * `reasoning`, `command_execution`, `file_change`, `result`, `error`). Each harness emits different
 * raw JSON shapes, so this module converts them into those canonical events without introducing a
 * heavy abstraction layer.
 *
 * Ported 1:1 from src/api/harness_events.py.
 */

import { asString, asRecord } from './parse-utils'
import type { CanonicalEvent, SubagentActivity } from './types'

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

function asList(value: unknown): unknown[] {
  return Array.isArray(value) ? value : []
}

const CODEX_PASSTHROUGH_EVENT_TYPES = [
  'turn.plan.updated',
  'item.started',
  'item.updated',
  'item.completed',
  'item.agentMessage.delta',
  'item.plan.delta',
  'item.commandExecution.outputDelta',
  'item.fileChange.outputDelta',
  'item.fileChange.patchUpdated',
  'item.reasoning.summaryTextDelta',
  'item.reasoning.summaryPartAdded',
  'item.reasoning.textDelta'
] as const

function isCodexPassthroughEventType(
  value: string
): value is (typeof CODEX_PASSTHROUGH_EVENT_TYPES)[number] {
  return CODEX_PASSTHROUGH_EVENT_TYPES.includes(
    value as (typeof CODEX_PASSTHROUGH_EVENT_TYPES)[number]
  )
}

function parseDictish(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  if (typeof value === 'string') {
    try {
      const parsed = JSON.parse(value)
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return parsed as Record<string, unknown>
      }
    } catch {
      return {}
    }
  }
  return {}
}

function stableToolCallId(name: string, toolInput: unknown, nonce: string = ''): string {
  const payload = {
    name: name || 'tool',
    input: toolInput && typeof toolInput === 'object' ? toolInput : {},
    nonce: nonce || ''
  }
  // Stable JSON with sorted keys (matching Python's sort_keys=True)
  const stableJson = stableSortedStringify(payload)
  const hash = sha1Hex(stableJson).slice(0, 12)
  return `tool-call-${hash}`
}

function stableSortedStringify(value: unknown): string {
  if (value === null || value === undefined) return 'null'
  if (typeof value === 'string') return JSON.stringify(value)
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (Array.isArray(value)) {
    return '[' + value.map(item => stableSortedStringify(item)).join(', ') + ']'
  }
  if (typeof value === 'object') {
    const record = value as Record<string, unknown>
    const keys = Object.keys(record).sort()
    const parts = keys.map(key => `${JSON.stringify(key)}: ${stableSortedStringify(record[key])}`)
    return '{' + parts.join(', ') + '}'
  }
  return JSON.stringify(value)
}

/**
 * Simple SHA-1 hex digest. Uses Node.js crypto when available, falls back to Web Crypto (which is
 * available in Next.js edge and browser).
 */
function sha1Hex(input: string): string {
  // In Next.js / Node.js environment, use the built-in crypto module
  const crypto = require('crypto') as typeof import('crypto')
  return crypto.createHash('sha1').update(input, 'utf-8').digest('hex')
}

// ---------------------------------------------------------------------------
// Factory helpers
// ---------------------------------------------------------------------------

function assistantTextEvent(text: string): CanonicalEvent {
  return { type: 'assistant', message: { content: [{ type: 'text', text }] } }
}

function assistantToolUseEvent(
  toolCallId: string,
  name: string,
  toolInput: unknown
): CanonicalEvent {
  const toolName = asString(name) || 'tool'
  const normalizedInput =
    toolInput && typeof toolInput === 'object' && !Array.isArray(toolInput)
      ? (toolInput as Record<string, unknown>)
      : {}
  const resolvedId = asString(toolCallId).trim() || stableToolCallId(toolName, normalizedInput)
  return {
    type: 'assistant',
    message: {
      content: [{ type: 'tool_use', id: resolvedId, name: toolName, input: normalizedInput }]
    }
  }
}

function toolResultEvent(
  toolUseId: string,
  content: unknown,
  isError: boolean = false
): CanonicalEvent {
  return {
    type: 'tool',
    content: [{ tool_use_id: toolUseId, content, is_error: isError }]
  }
}

function subagentEvent(opts: {
  status: string
  subagent_id: string
  name?: string
  summary?: string
  error?: string
  activity?: string
  activities?: SubagentActivity[]
}): CanonicalEvent {
  const payload: CanonicalEvent & { type: 'subagent' } = {
    type: 'subagent',
    status: opts.status,
    subagent_id: opts.subagent_id
  }
  if (opts.name) payload.name = opts.name
  if (opts.summary) payload.summary = opts.summary
  if (opts.error) payload.error = opts.error
  if (opts.activity) payload.activity = opts.activity
  if (opts.activities?.length) payload.activities = opts.activities
  return payload
}

function normalizeSubagentStatusString(raw: string): string {
  const s = raw.trim().toLowerCase()
  if (s === 'started' || s === 'start' || s === 'starting') return 'started'
  if (s === 'working' || s === 'running' || s === 'in_progress' || s === 'progress')
    return 'working'
  if (s === 'completed' || s === 'done' || s === 'complete' || s === 'finished' || s === 'success')
    return 'completed'
  if (s === 'failed' || s === 'error' || s === 'failure') return 'failed'
  return raw
}

function firstNonEmptyString(...values: unknown[]): string | undefined {
  for (const value of values) {
    const normalized = asString(value).trim()
    if (normalized) return normalized
  }
  return undefined
}

function makeActivity(description: unknown, toolName?: unknown): SubagentActivity | undefined {
  const text = asString(description).trim()
  if (!text) return undefined
  const tool = asString(toolName).trim()
  return tool ? { description: text, toolName: tool } : { description: text }
}

function mergeActivities(
  ...items: Array<SubagentActivity | undefined>
): SubagentActivity[] | undefined {
  const merged: SubagentActivity[] = []
  const seen = new Set<string>()
  for (const item of items) {
    if (!item) continue
    const key = `${item.toolName ?? ''}::${item.description}`
    if (seen.has(key)) continue
    seen.add(key)
    merged.push(item)
  }
  return merged.length > 0 ? merged : undefined
}

// ---------------------------------------------------------------------------
// Usage metadata
// ---------------------------------------------------------------------------

function usagePayloadFromSource(
  source: Record<string, unknown>
): [Record<string, unknown> | null, string | null] {
  const message = asRecord(source.message)
  const usage =
    Object.keys(asRecord(message.usage)).length > 0
      ? asRecord(message.usage)
      : Object.keys(asRecord(source.usage)).length > 0
        ? asRecord(source.usage)
        : null
  if (!usage) return [null, null]
  const model = asString(message.model) || asString(source.model) || null
  return [usage, model]
}

function attachUsageMetadata(
  events: CanonicalEvent[],
  source: Record<string, unknown>,
  authoritative: boolean = false
): CanonicalEvent[] {
  const [usage, model] = usagePayloadFromSource(source)
  if (!usage) return events

  if (events.length === 0) {
    const passthrough: CanonicalEvent & { type: 'usage' } = {
      type: 'usage',
      usage
    }
    if (model) passthrough.model = model
    if (authoritative) passthrough.authoritative = true
    return [passthrough]
  }

  const first = { ...events[0] } as Record<string, unknown>
  const message = asRecord(first.message)
  if (Object.keys(message).length > 0) {
    const updatedMessage: Record<string, unknown> = { ...message, usage }
    if (model) updatedMessage.model = model
    first.message = updatedMessage
  } else {
    first.usage = usage
    if (model) first.model = model
  }
  if (authoritative) first.authoritative = true
  return [first as CanonicalEvent, ...events.slice(1)]
}

// ---------------------------------------------------------------------------
// Amp / Claude-Code normalizer
// ---------------------------------------------------------------------------

function normalizeAmpLikeEvent(event: Record<string, unknown>): CanonicalEvent[] {
  const eventType = asString(event.type)

  if (eventType === 'user') {
    const message = asRecord(event.message)
    const toolResults: Array<{ tool_use_id: string; content: unknown; is_error: boolean }> = []
    for (const block of asList(message.content)) {
      const blockDict = asRecord(block)
      if (asString(blockDict.type) !== 'tool_result') continue
      const toolUseId = asString(blockDict.tool_use_id) || asString(event.parent_tool_use_id)
      if (!toolUseId) continue
      toolResults.push({
        tool_use_id: toolUseId,
        content: blockDict.content,
        is_error: Boolean(blockDict.is_error)
      })
    }
    if (toolResults.length > 0) {
      return [{ type: 'tool', content: toolResults }]
    }
    return []
  }

  if (
    eventType === 'assistant' ||
    eventType === 'reasoning' ||
    eventType === 'tool' ||
    eventType === 'command_execution' ||
    eventType === 'file_change'
  ) {
    return [event as unknown as CanonicalEvent]
  }

  if (eventType === 'subagent') {
    const status = asString(event.status).trim()
    const subagentId = firstNonEmptyString(
      event.subagent_id,
      event.task_id,
      event.tool_use_id,
      event.id
    )
    if (!status || !subagentId) return [event as unknown as CanonicalEvent]
    const description = firstNonEmptyString(event.description, event.message)
    const toolName = firstNonEmptyString(
      event.tool_name,
      event.toolName,
      event.last_tool_name,
      event.lastToolName,
      event.active_tool_name,
      event.activeToolName
    )
    return [
      subagentEvent({
        status: normalizeSubagentStatusString(status),
        subagent_id: subagentId,
        name: firstNonEmptyString(event.name, event.task_name, description),
        summary: firstNonEmptyString(event.summary, event.result),
        error: firstNonEmptyString(event.error),
        activity: description,
        activities: mergeActivities(makeActivity(description, toolName))
      })
    ]
  }

  if (eventType === 'result') {
    const text = asString(event.result) || asString(event.text)
    return text ? [{ type: 'result', text }] : []
  }

  if (eventType === 'error') {
    const message =
      asString(event.error) ||
      asString(asRecord(event.error).message) ||
      asString(event.message) ||
      'Unknown error'
    const lowered = message.toLowerCase()
    if (lowered.includes('restarting (') && !lowered.includes('giving up')) {
      return []
    }
    return [{ type: 'error', error: message }]
  }

  if (eventType === 'system') {
    const subtype = asString(event.subtype).trim().toLowerCase()
    const subagentId = firstNonEmptyString(
      event.task_id,
      event.subagent_id,
      event.tool_use_id,
      event.parent_tool_use_id,
      event.id
    )
    if (!subagentId) {
      if (subtype === 'init') {
        const sessionId = firstNonEmptyString(event.session_id)
        return sessionId ? [{ type: 'system', subtype: 'init', session_id: sessionId }] : []
      }
      return []
    }
    const description = firstNonEmptyString(event.description, event.message, event.text)
    const summary = firstNonEmptyString(event.summary, event.result, event.message, event.text)
    const name = firstNonEmptyString(event.name, event.task_name, event.title, description)
    const toolName = firstNonEmptyString(
      event.tool_name,
      event.toolName,
      event.last_tool_name,
      event.lastToolName,
      event.active_tool_name,
      event.activeToolName
    )
    const activities = mergeActivities(makeActivity(description, toolName))

    if (subtype === 'task_started' || subtype === 'task_start' || subtype === 'started') {
      return [
        subagentEvent({
          status: 'started',
          subagent_id: subagentId,
          name: name ?? 'Delegated task',
          activity: description,
          activities
        })
      ]
    }
    if (
      subtype === 'task_progress' ||
      subtype === 'task_update' ||
      subtype === 'progress' ||
      subtype === 'working'
    ) {
      return [
        subagentEvent({
          status: 'working',
          subagent_id: subagentId,
          name: name,
          activity: description,
          activities
        })
      ]
    }
    if (
      subtype === 'task_notification' ||
      subtype === 'task_completed' ||
      subtype === 'task_done' ||
      subtype === 'completed' ||
      subtype === 'done'
    ) {
      return [
        subagentEvent({
          status: 'completed',
          subagent_id: subagentId,
          name,
          summary: summary ?? description,
          activity: description,
          activities
        })
      ]
    }
    if (
      subtype === 'task_failed' ||
      subtype === 'task_error' ||
      subtype === 'failed' ||
      subtype === 'error'
    ) {
      return [
        subagentEvent({
          status: 'failed',
          subagent_id: subagentId,
          name,
          error: firstNonEmptyString(event.error, event.message) || 'Task failed'
        })
      ]
    }
    return []
  }

  if (eventType === 'stream_event') {
    const nested = asRecord(event.event)
    const nestedType = asString(nested.type)
    if (nestedType === 'error') {
      const msg = asString(asRecord(nested.error).message) || 'Unknown error'
      return [{ type: 'error', error: msg }]
    }
    if (nestedType === 'content_block_start') {
      const block = asRecord(nested.content_block)
      if (asString(block.type) === 'tool_use') {
        const toolId = asString(block.id)
        const name = asString(block.name) || 'tool'
        return [assistantToolUseEvent(toolId, name, block.input)]
      }
    }
    if (nestedType === 'content_block_delta') {
      const delta = asRecord(nested.delta)
      const deltaType = asString(delta.type)
      if (deltaType === 'text_delta') {
        const text = asString(delta.text)
        return text ? [assistantTextEvent(text)] : []
      }
      if (deltaType === 'thinking_delta') {
        const text = asString(delta.thinking)
        return text ? [{ type: 'reasoning', text }] : []
      }
    }
    return []
  }

  return []
}

// ---------------------------------------------------------------------------
// Codex normalizer
// ---------------------------------------------------------------------------

function codexToolName(item: Record<string, unknown>): string {
  return (
    asString(item.tool) ||
    asString(item.toolName) ||
    asString(item.name) ||
    asString(item.tool_name) ||
    'tool'
  )
}

function codexToolInput(item: Record<string, unknown>): Record<string, unknown> {
  for (const key of ['arguments', 'input', 'args']) {
    const value = parseDictish(item[key])
    if (Object.keys(value).length > 0) return value
  }
  return {}
}

function codexToolCallId(item: Record<string, unknown>): string {
  const directId =
    asString(item.id) ||
    asString(item.tool_call_id) ||
    asString(item.tool_use_id) ||
    asString(item.toolUseId) ||
    asString(item.toolCallId) ||
    asString(item.call_id)
  if (directId) return directId
  const nonce =
    asString(item.index) ||
    asString(item.position) ||
    asString(item.ordinal) ||
    asString(item.event_seq) ||
    asString(item.timestamp) ||
    asString(item.created_at)
  return stableToolCallId(codexToolName(item), codexToolInput(item), nonce)
}

function normalizeCodexItem(item: Record<string, unknown>, phase: string): CanonicalEvent[] {
  const itemType = asString(item.type)

  if ((itemType === 'agent_message' || itemType === 'agentMessage') && phase === 'completed') {
    return []
  }

  if (itemType === 'reasoning' && (phase === 'updated' || phase === 'completed')) {
    const text = asString(item.text) || asString(item.thinking)
    return text ? [{ type: 'reasoning', text }] : []
  }

  if (
    itemType === 'mcp_tool_call' ||
    itemType === 'mcpToolCalls' ||
    itemType === 'tool_call' ||
    itemType === 'toolCall' ||
    itemType === 'function_call' ||
    itemType === 'functionCall' ||
    itemType === 'custom_tool_call' ||
    itemType === 'customToolCall' ||
    itemType === 'dynamicToolCalls' ||
    itemType === 'collabToolCalls'
  ) {
    const toolId = codexToolCallId(item)
    const toolName = codexToolName(item)
    if (toolName.trim().toLowerCase() === 'subagent') {
      const toolInput = codexToolInput(item)
      const label =
        asString(toolInput.description) || asString(toolInput.name) || 'Delegated subagent'
      if (phase === 'started') {
        return [subagentEvent({ status: 'started', subagent_id: toolId, name: label })]
      }
      if (phase === 'updated') {
        const activity = firstNonEmptyString(
          item.message,
          item.status_message,
          item.progress_message,
          item.summary
        )
        return activity
          ? [
              subagentEvent({
                status: 'working',
                subagent_id: toolId,
                name: label,
                activity,
                activities: mergeActivities(
                  makeActivity(
                    activity,
                    firstNonEmptyString(
                      item.active_tool_name,
                      item.activeToolName,
                      item.last_tool_name,
                      item.lastToolName
                    )
                  )
                )
              })
            ]
          : []
      }
      if (phase === 'completed') {
        if (item.error !== undefined && item.error !== null) {
          return [
            subagentEvent({
              status: 'failed',
              subagent_id: toolId,
              name: label,
              error: asString(item.error) || 'Subagent failed'
            })
          ]
        }
        const resultSummary = asString(item.result)
        return [
          subagentEvent({
            status: 'completed',
            subagent_id: toolId,
            name: label,
            summary: resultSummary.slice(0, 220)
          })
        ]
      }
      return []
    }
    if (phase === 'started') {
      const toolInput = codexToolInput(item)
      return [assistantToolUseEvent(toolId, toolName, toolInput)]
    }
    if (phase === 'completed') {
      let output = item.result
      if (output === undefined && item.error !== undefined && item.error !== null) {
        output = item.error
      }
      return [toolResultEvent(toolId, output, Boolean(item.error))]
    }
    return []
  }

  if (itemType === 'command_execution' || itemType === 'commandExecution') {
    const command = asString(item.command)
    if (phase === 'completed') {
      return [
        {
          type: 'command_execution',
          command,
          aggregated_output: (item.aggregated_output as string) || (item.output as string) || '',
          exit_code: item.exit_code,
          status: item.status
        }
      ]
    }
    return []
  }

  if ((itemType === 'file_change' || itemType === 'fileChange') && phase === 'completed') {
    const changes = item.changes
    return [{ type: 'file_change', changes: Array.isArray(changes) ? changes : [] }]
  }

  if (itemType === 'error') {
    const message = asString(item.message) || 'Unknown error'
    return [{ type: 'error', error: message }]
  }

  return []
}

function normalizeCodexEvent(event: Record<string, unknown>): CanonicalEvent[] {
  const eventType = asString(event.type)

  if (eventType === 'thread.started') {
    const threadId = asString(event.thread_id)
    return threadId ? [{ type: 'system', subtype: 'init', session_id: threadId }] : []
  }

  if (eventType === 'assistant') {
    return [event]
  }

  if (eventType === 'error') {
    const error = asRecord(event.error)
    const message =
      asString(event.error) ||
      asString(error.message) ||
      asString(event.message) ||
      'Unknown error'
    return [{ type: 'error', error: message }]
  }

  if (eventType === 'turn.failed') {
    const error = asRecord(event.error)
    const message = asString(error.message) || asString(event.message) || 'Turn failed'
    return [{ type: 'error', error: message }]
  }

  if (eventType === 'turn.completed') {
    return attachUsageMetadata([], event, true)
  }

  if (
    eventType === 'item.started' ||
    eventType === 'item.updated' ||
    eventType === 'item.completed'
  ) {
    const item = asRecord(event.item)
    if (asString(item.type) === 'error') {
      return normalizeCodexItem(item, eventType.split('.').at(-1) ?? '')
    }
  }

  if (isCodexPassthroughEventType(eventType)) {
    return [{ ...event, type: eventType }]
  }

  return []
}

// ---------------------------------------------------------------------------
// Pi-mono normalizer
// ---------------------------------------------------------------------------

function normalizePiMessageContent(message: Record<string, unknown>): CanonicalEvent[] {
  const content = asList(message.content)
  const normalized: CanonicalEvent[] = []
  for (const block of content) {
    const blockDict = asRecord(block)
    const blockType = asString(blockDict.type)
    if (blockType === 'text') {
      const text = asString(blockDict.text)
      if (text) normalized.push(assistantTextEvent(text))
    } else if (blockType === 'thinking') {
      const text = asString(blockDict.text) || asString(blockDict.thinking)
      if (text) normalized.push({ type: 'reasoning', text })
    } else if (blockType === 'tool_call' || blockType === 'toolcall') {
      const toolCall = (() => {
        const tc = asRecord(blockDict.toolCall)
        return Object.keys(tc).length > 0 ? tc : blockDict
      })()
      const toolName = asString(toolCall.name) || asString(blockDict.name) || 'tool'
      const toolInput = (() => {
        const ti = asRecord(toolCall.input)
        return Object.keys(ti).length > 0 ? ti : asRecord(blockDict.input)
      })()
      const toolId = asString(toolCall.id) || asString(blockDict.id)
      normalized.push(assistantToolUseEvent(toolId, toolName, toolInput))
    }
  }
  return normalized
}

function normalizePiEvent(event: Record<string, unknown>): CanonicalEvent[] {
  const eventType = asString(event.type)

  if (eventType === 'session') {
    const sessionId = asString(event.id)
    return sessionId ? [{ type: 'system', subtype: 'init', session_id: sessionId }] : []
  }

  if (eventType === 'tool_execution_start') {
    const toolName = asString(event.toolName) || 'tool'
    const toolInput = asRecord(event.args)
    let toolId = asString(event.toolCallId)
    if (!toolId) {
      const nonce =
        asString(event.toolExecutionId) || asString(event.executionId) || asString(event.id)
      toolId = stableToolCallId(toolName, toolInput, nonce)
    }
    if (toolName.trim().toLowerCase() === 'subagent') {
      const label =
        asString(toolInput.description) || asString(toolInput.name) || 'Delegated subagent'
      return [subagentEvent({ status: 'started', subagent_id: toolId, name: label })]
    }
    return [assistantToolUseEvent(toolId, toolName, toolInput)]
  }

  if (eventType === 'tool_execution_end') {
    let toolId = asString(event.toolCallId)
    if (!toolId) {
      const toolName = asString(event.toolName) || 'tool'
      const toolInput = asRecord(event.args)
      const nonce =
        asString(event.toolExecutionId) || asString(event.executionId) || asString(event.id)
      if (!nonce) return []
      toolId = stableToolCallId(toolName, toolInput, nonce)
    }
    if (asString(event.toolName).trim().toLowerCase() === 'subagent') {
      if (Boolean(event.isError)) {
        return [
          subagentEvent({
            status: 'failed',
            subagent_id: toolId,
            error: asString(event.error) || 'Subagent failed'
          })
        ]
      }
      const resultSummary = asString(event.result)
      return [
        subagentEvent({
          status: 'completed',
          subagent_id: toolId,
          summary: resultSummary.slice(0, 220)
        })
      ]
    }
    return [toolResultEvent(toolId, event.result, Boolean(event.isError))]
  }

  if (eventType === 'tool_execution_update') {
    const toolName = asString(event.toolName) || 'tool'
    let toolId = asString(event.toolCallId)
    if (!toolId) {
      const toolInput = asRecord(event.args)
      const nonce =
        asString(event.toolExecutionId) || asString(event.executionId) || asString(event.id)
      toolId = stableToolCallId(toolName, toolInput, nonce)
    }
    if (toolName.trim().toLowerCase() !== 'subagent') return []
    const toolInput = asRecord(event.args)
    const label =
      asString(toolInput.description) || asString(toolInput.name) || 'Delegated subagent'
    const activity = firstNonEmptyString(
      event.message,
      event.statusMessage,
      event.progress_message,
      event.summary
    )
    return activity
      ? [
          subagentEvent({
            status: 'working',
            subagent_id: toolId,
            name: label,
            activity,
            activities: mergeActivities(
              makeActivity(
                activity,
                firstNonEmptyString(
                  event.active_tool_name,
                  event.activeToolName,
                  event.last_tool_name,
                  event.lastToolName
                )
              )
            )
          })
        ]
      : []
  }

  if (eventType === 'message_end') {
    const message = asRecord(event.message)
    const role = asString(message.role)
    if (role !== 'assistant') return []
    const normalized = attachUsageMetadata(normalizePiMessageContent(message), message)
    const stopReason = asString(message.stopReason)
    if (stopReason === 'error' || stopReason === 'aborted') {
      const errorText = asString(message.errorMessage) || 'Assistant run failed'
      return [...normalized, { type: 'error', error: errorText }]
    }
    return normalized
  }

  if (eventType === 'agent_end') {
    const messages = asList(event.messages)
    if (messages.length === 0) return []
    const assistantMessages = messages.filter(m => asString(asRecord(m).role) === 'assistant')
    if (assistantMessages.length === 0) return []
    const lastAssistant = asRecord(assistantMessages[assistantMessages.length - 1])
    const stopReason = asString(lastAssistant.stopReason)
    if (stopReason === 'error' || stopReason === 'aborted') {
      const errorText = asString(lastAssistant.errorMessage) || 'Assistant run failed'
      return [{ type: 'error', error: errorText }]
    }
    return []
  }

  return []
}

// ---------------------------------------------------------------------------
// Main dispatcher
// ---------------------------------------------------------------------------

export function normalizeHarnessEvent(
  harness: string,
  event: Record<string, unknown>
): CanonicalEvent[] {
  let normalizedHarness = (harness || '').trim().toLowerCase()

  if (!normalizedHarness) {
    const eventType = asString(event.type)
    if (
      eventType.startsWith('item.') ||
      eventType.startsWith('turn.') ||
      eventType === 'thread.started'
    ) {
      normalizedHarness = 'codex'
    } else if (
      eventType === 'session' ||
      eventType === 'agent_start' ||
      eventType === 'agent_end' ||
      eventType === 'message_start' ||
      eventType === 'message_update' ||
      eventType === 'message_end' ||
      eventType === 'tool_execution_start' ||
      eventType === 'tool_execution_update' ||
      eventType === 'tool_execution_end'
    ) {
      normalizedHarness = 'pi-mono'
    } else {
      normalizedHarness = 'amp'
    }
  }

  if (normalizedHarness === 'codex') {
    return normalizeCodexEvent(event)
  }
  if (normalizedHarness === 'pi-mono') {
    return normalizePiEvent(event)
  }
  // eng/legal use claude-code under the hood, same event format as amp/claude-code
  return normalizeAmpLikeEvent(event)
}
