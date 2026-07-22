import type { RustSessionStreamEvent } from '@centaur/harness-events'
import {
  ChatSDKRenderer,
  type ChatSDKOutput,
  type ChatSDKStreamChunk
} from './chat-sdk'
import {
  CodexAppServerRendererEventMapper,
  type CodexAppServerToChatStreamOptions
} from './codex-app-server'
import type {
  RendererEvent,
  RendererSourceMapper,
  RendererTaskStatus
} from './types'

type NativeEvent = {
  protocol_version: number
  request_id: string
  seq: number
  type: string
  payload: Record<string, unknown>
}

export class NanocodexRendererEventMapper
  implements RendererSourceMapper<RustSessionStreamEvent | unknown>
{
  private answer = ''
  private error = ''
  private requestId = ''
  private done = false

  process(source: RustSessionStreamEvent | unknown): RendererEvent[] {
    if (this.done) return []
    const event = nanocodexEvent(source)
    if (!event) return this.processSessionEnvelope(source)
    this.requestId = event.request_id || this.requestId

    switch (event.type) {
      case 'assistant.delta': {
        if (stringField(event.payload, 'phase') === 'commentary') return []
        const delta = stringField(event.payload, 'text')
        if (!delta) return []
        this.answer += delta
        return [{ type: 'renderer.message.delta', delta }]
      }
      case 'assistant.message': {
        if (stringField(event.payload, 'phase') === 'commentary') {
          const text = stringField(event.payload, 'text')
          if (!text) return []
          return [
            {
              type: 'renderer.task.update',
              task: {
                id: stringField(event.payload, 'item_id') || `commentary-${event.seq}`,
                title: 'Thinking',
                status: 'complete',
                details: [{ type: 'text', text }]
              }
            }
          ]
        }
        const markdown = stringField(event.payload, 'text')
        if (!markdown || markdown === this.answer) return []
        if (markdown.startsWith(this.answer)) {
          const delta = markdown.slice(this.answer.length)
          this.answer = markdown
          return delta ? [{ type: 'renderer.message.delta', delta }] : []
        }
        this.answer = markdown
        return [{ type: 'renderer.message.snapshot', markdown }]
      }
      case 'tool.call':
        return [
          {
            type: 'renderer.task.update',
            task: {
              id: stringField(event.payload, 'call_id') || `tool-${event.seq}`,
              title: stringField(event.payload, 'tool') || 'Tool',
              status: 'in_progress',
              details: blocks(event.payload.arguments)
            }
          }
        ]
      case 'tool.result': {
        const status = toolStatus(event.payload)
        return [
          {
            type: 'renderer.task.update',
            task: {
              id: stringField(event.payload, 'call_id') || `tool-${event.seq}`,
              title: stringField(event.payload, 'tool') || 'Tool',
              status,
              output: blocks(event.payload.result)
            }
          }
        ]
      }
      case 'run.error':
        this.error = stringField(event.payload, 'message') || 'Nanocodex run failed'
        return []
      case 'run.completed':
        return this.complete()
      case 'run.failed':
        if (['cancelled', 'canceled'].includes(stringField(event.payload, 'status'))) {
          return this.interrupt()
        }
        return this.fail(this.error || 'Nanocodex run failed')
      default:
        return []
    }
  }

  flush(): RendererEvent[] {
    return this.done ? [] : this.complete()
  }

  isDone(): boolean {
    return this.done
  }

  threadId(): string {
    return this.requestId
  }

  private processSessionEnvelope(source: unknown): RendererEvent[] {
    if (!isRecord(source)) return []
    const kind = String(source.eventKind ?? source.event ?? '')
    const data = isRecord(source.data) ? source.data : source
    if (kind === 'session.activity_summary') {
      const status = String(data.summary ?? data.status ?? '').trim()
      return status ? [{ type: 'renderer.status', status }] : []
    }
    if (
      kind === 'session.execution_failed' ||
      kind === 'session.stream_error' ||
      kind === 'session.stdout_pump_failed'
    ) {
      return this.fail(String(data.error ?? 'Execution failed'))
    }
    if (kind === 'session.execution_cancelled') return this.interrupt()
    if (kind === 'session.execution_completed') return this.complete()
    return []
  }

  private interrupt(): RendererEvent[] {
    if (!this.answer) this.answer = 'Execution interrupted'
    return this.complete()
  }

  private complete(): RendererEvent[] {
    if (this.done) return []
    this.done = true
    return [
      {
        type: 'renderer.done',
        answerMarkdown: this.answer,
        streamFinalUpdates: true,
        threadId: this.requestId || undefined
      }
    ]
  }

  private fail(error: string): RendererEvent[] {
    if (this.done) return []
    this.done = true
    return [
      {
        type: 'renderer.done',
        answerMarkdown: this.answer || undefined,
        error,
        streamFinalUpdates: true,
        threadId: this.requestId || undefined
      }
    ]
  }
}

export async function* harnessToChatSdkStream(
  sources: AsyncIterable<RustSessionStreamEvent | unknown>,
  options: CodexAppServerToChatStreamOptions = {}
): AsyncIterable<ChatSDKStreamChunk> {
  const codex = new CodexAppServerRendererEventMapper(options)
  const nano = new NanocodexRendererEventMapper()
  const renderer = new ChatSDKRenderer()
  let selected: 'codex' | 'nanocodex' | null = null

  for await (const source of sources) {
    if (nanocodexEvent(source)) selected = 'nanocodex'
    const mapper = selected === 'nanocodex' ? nano : codex
    for (const event of mapper.process(source)) {
      yield* render(renderer, mapper.threadId(), event, options)
    }
    if (mapper.isDone()) return
  }

  const mapper = selected === 'nanocodex' ? nano : codex
  for (const event of mapper.flush()) {
    yield* render(renderer, mapper.threadId(), event, options)
  }
}

export function isNanocodexEvent(source: unknown): boolean {
  return nanocodexEvent(source) !== null
}

function nanocodexEvent(source: unknown): NativeEvent | null {
  let candidate = source
  if (isRecord(source)) {
    const kind = String(source.eventKind ?? source.event ?? '')
    if (kind === 'session.output.line') {
      const data = source.data
      if (typeof data === 'string') {
        try {
          candidate = JSON.parse(data)
        } catch {
          return null
        }
      } else if (isRecord(data)) {
        candidate = data.raw ?? data
        if (typeof candidate === 'string') {
          try {
            candidate = JSON.parse(candidate)
          } catch {
            return null
          }
        }
      }
    }
  }
  if (!isRecord(candidate)) return null
  if (candidate.protocol_version !== 1 || typeof candidate.request_id !== 'string') return null
  if (typeof candidate.type !== 'string' || !isRecord(candidate.payload)) return null
  return candidate as NativeEvent
}

async function* render(
  renderer: ChatSDKRenderer,
  sessionId: string,
  event: RendererEvent,
  options: CodexAppServerToChatStreamOptions
): AsyncIterable<ChatSDKStreamChunk> {
  await options.onRendererEvent?.(event)
  const outputs = renderer.render(sessionId, event)
  for (const output of outputs) {
    await options.onOutput?.(output as ChatSDKOutput, event)
    if (output.type !== 'chat.stream.append') continue
    for (const chunk of output.chunks) yield chunk
  }
}

function blocks(value: unknown): Array<{ type: 'code'; text: string; language: string }> {
  if (value === undefined || value === null) return []
  const text = typeof value === 'string' ? value : JSON.stringify(value, null, 2)
  return text ? [{ type: 'code', text, language: 'json' }] : []
}

function toolStatus(payload: Record<string, unknown>): RendererTaskStatus {
  const status = stringField(payload, 'status').toLowerCase()
  return status === 'failed' || status === 'error' ? 'error' : 'complete'
}

function stringField(value: Record<string, unknown>, key: string): string {
  const field = value[key]
  return typeof field === 'string' ? field : ''
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
