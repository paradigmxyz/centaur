import type { AnyBlock, AnyChunk } from '@slack/types'
import type { WebClient } from '@slack/web-api'
import { ulid } from '@std/ulid'
import {
  markdownChunk,
  planBlock,
  planUpdateChunk,
  preformatted,
  richText,
  taskUpdateChunk,
  type StreamRichText,
  type StreamTask,
  type StreamTaskStatus
} from './streaming'
import { renderMarkdownBlocks } from './render'

type Segment = {
  id: string
  textParts: string[]
  tasks: Map<string, StreamTask>
  streamTs?: string
  planStarted: boolean
  pendingText: string
  streamedText: string
  pendingTextTimer?: ReturnType<typeof setTimeout>
  pendingTextFlush?: Promise<void>
  streamError?: Error
  closed: boolean
}

type AgentSessionState = {
  id: string
  channel: string
  parentTs: string
  recipientTeamId: string
  recipientUserId: string
  title: string
  footer?: string
  done: boolean
  statusCleared: boolean
  segments: Segment[]
}

export type OpenAgentSessionInput = {
  channel: string
  parentTs: string
  recipientTeamId: string
  recipientUserId: string
  title?: string
}

export type StepInput = {
  id: string
  title: string
  status?: StreamTaskStatus
  details?: StreamRichText | string
  output?: StreamRichText | string
}

export type StepOptions = {
  flush?: boolean
}

export type TextOptions = {
  flush?: boolean
}

export type DoneOptions = {
  streamFinalUpdates?: boolean
}

const sessions = new Map<string, AgentSessionState>()
const THINKING_STATUS = 'Thinking...'
const TEXT_FLUSH_INTERVAL_MS = 250
const TEXT_FLUSH_CHARS = 1000
const FIRST_TEXT_FLUSH_CHARS = 1
const EXECUTION_PLAN_ID = 'codex-execution-timeline'
const FINAL_PLAN_MAX_TASKS = 12
const FINAL_PLAN_TITLE_CHARS = 140
const FINAL_PLAN_DETAILS_CHARS = 192
const FINAL_PLAN_OUTPUT_CHARS = 256

export class AgentSessionRenderer {
  constructor(private readonly client: WebClient) {}

  async open(input: OpenAgentSessionInput): Promise<{ sessionId: string }> {
    const id = ulid()
    sessions.set(id, {
      id,
      channel: input.channel,
      parentTs: input.parentTs,
      recipientTeamId: input.recipientTeamId,
      recipientUserId: input.recipientUserId,
      title: input.title ?? 'Execution steps',
      segments: [newSegment()],
      done: false,
      statusCleared: false
    })
    await this.setStatus(id, THINKING_STATUS)
    return { sessionId: id }
  }

  async text(sessionId: string, markdown: string): Promise<void> {
    const state = requireSession(sessionId)
    const segment = currentSegment(state)

    segment.textParts.push(markdown)
    await this.queueText(state, segment, markdown)
  }

  async textDelta(
    sessionId: string,
    markdownDelta: string,
    opts: TextOptions = {}
  ): Promise<void> {
    if (!markdownDelta) return
    const state = requireSession(sessionId)
    const segment = currentSegment(state)

    const lastIndex = segment.textParts.length - 1
    if (lastIndex >= 0) segment.textParts[lastIndex] += markdownDelta
    else segment.textParts.push(markdownDelta)
    if (opts.flush === false) {
      segment.pendingText += normalizeDeltaBoundary(
        segment.streamedText + segment.pendingText,
        markdownDelta
      )
      return
    }
    await this.queueText(state, segment, markdownDelta)
  }

  async step(sessionId: string, input: StepInput, opts: StepOptions = {}): Promise<void> {
    const state = requireSession(sessionId)
    const segment = currentSegment(state)
    const existing = segment.tasks.get(input.id)
    const task = {
      id: input.id,
      title: input.title,
      status: input.status ?? existing?.status ?? 'in_progress',
      details: input.details ?? existing?.details,
      output: input.output ?? existing?.output
    }
    const storedTask = { ...task }
    segment.tasks.set(storedTask.id, storedTask)
    const taskUpdate = {
      id: storedTask.id,
      title: storedTask.title,
      status: storedTask.status,
      details: input.details !== undefined ? storedTask.details : undefined,
      output: input.output !== undefined ? storedTask.output : undefined
    }
    if (opts.flush === false) return
    await this.flushTask(state, segment, taskUpdate)
    await this.flushText(state, segment, { force: true })
  }

  async done(sessionId: string, footer?: string, opts: DoneOptions = {}): Promise<void> {
    const state = requireSession(sessionId)
    state.done = true
    state.footer = footer
    const streamFinalUpdates = opts.streamFinalUpdates ?? true
    let closed = false

    try {
      for (const segment of state.segments) {
        balancePendingMarkdown(segment)
        if (streamFinalUpdates) {
          await this.flushText(state, segment, { force: true })
        } else {
          await this.absorbPendingText(segment)
        }
        const finalizedTasks = finalizeOpenTasks(segment)
        if (streamFinalUpdates) {
          for (const task of finalizedTasks) {
            await this.flushTask(state, segment, task)
          }
        }
        await this.closeTextStream(state, segment)
      }
      closed = true
    } finally {
      await this.setStatus(sessionId, '')
      if (closed) sessions.delete(sessionId)
    }
  }

  private async setStatus(sessionId: string, status: string): Promise<void> {
    const state = requireSession(sessionId)
    const response = await this.client.assistant.threads.setStatus({
      channel_id: state.channel,
      thread_ts: state.parentTs,
      status,
      ...(status ? { loading_messages: [status] } : {})
    })
    if (!response.ok) throw new Error(response.error ?? 'assistant.threads.setStatus failed')
  }

  private async closeTextStream(state: AgentSessionState, segment: Segment): Promise<void> {
    raiseStreamError(segment)
    if (segment.closed) return
    if (!segment.streamTs && !segment.textParts.length && !segment.tasks.size) return
    const footer = state.footer?.trim()
    await this.ensureStream(state, segment, [])
    if (!segment.streamTs) return
    const originalTasks = finalTaskSnapshot(segment)
    const tasks = compactFinalTasks(originalTasks)
    const blocks = [
      ...(tasks.length ? [planBlock(planTitle(state.title, originalTasks), tasks, EXECUTION_PLAN_ID)] : []),
      ...renderMarkdownBlocks(segment.streamedText),
      ...(footer ? footerBlocks(footer) : [])
    ] as AnyBlock[]
    const stopResponse = await this.client.chat.stopStream({
      channel: state.channel,
      ts: segment.streamTs
    })
    if (!stopResponse.ok) throw new Error(stopResponse.error ?? 'chat.stopStream failed')
    if (blocks.length) {
      const updateResponse = await this.client.chat.update({
        channel: state.channel,
        ts: segment.streamTs,
        text: fallbackTextForBlocks(state.title, segment.streamedText, footer),
        blocks
      })
      if (!updateResponse.ok) throw new Error(updateResponse.error ?? 'chat.update failed')
    }
    segment.closed = true
  }

  private async streamChunks(
    state: AgentSessionState,
    segment: Segment,
    chunks: AnyChunk[]
  ): Promise<void> {
    raiseStreamError(segment)
    if (!chunks.length || segment.closed) return
    if (!segment.streamTs) {
      await this.ensureStream(state, segment, chunks)
      return
    }
    const response = await this.client.chat.appendStream({
      channel: state.channel,
      ts: segment.streamTs,
      chunks
    })
    if (!response.ok) throw new Error(response.error ?? 'chat.appendStream failed')
    await this.clearStatusAfterVisibleOutput(state, chunks)
  }

  private async queueText(
    state: AgentSessionState,
    segment: Segment,
    markdown: string
  ): Promise<void> {
    raiseStreamError(segment)
    segment.pendingText += normalizeDeltaBoundary(
      segment.streamedText + segment.pendingText,
      markdown
    )
    if (segment.pendingText.length >= TEXT_FLUSH_CHARS) {
      await this.flushText(state, segment, { force: true })
      return
    }
    this.scheduleTextFlush(state, segment)
  }

  private scheduleTextFlush(state: AgentSessionState, segment: Segment): void {
    if (segment.pendingTextTimer || segment.closed) return
    segment.pendingTextTimer = setTimeout(() => {
      segment.pendingTextTimer = undefined
      segment.pendingTextFlush = this.flushTextNow(state, segment, { force: false })
        .catch(error => {
          segment.streamError = error instanceof Error ? error : new Error(String(error))
        })
        .finally(() => {
          segment.pendingTextFlush = undefined
        })
    }, TEXT_FLUSH_INTERVAL_MS)
  }

  private async flushText(
    state: AgentSessionState,
    segment: Segment,
    opts: { force?: boolean } = {}
  ): Promise<void> {
    raiseStreamError(segment)
    if (segment.pendingTextTimer) {
      clearTimeout(segment.pendingTextTimer)
      segment.pendingTextTimer = undefined
    }
    if (segment.pendingTextFlush) await segment.pendingTextFlush
    if (!segment.pendingText) return
    segment.pendingTextFlush = this.flushTextNow(state, segment, opts).finally(() => {
      segment.pendingTextFlush = undefined
    })
    await segment.pendingTextFlush
  }

  private async absorbPendingText(segment: Segment): Promise<void> {
    raiseStreamError(segment)
    if (segment.pendingTextTimer) {
      clearTimeout(segment.pendingTextTimer)
      segment.pendingTextTimer = undefined
    }
    if (segment.pendingTextFlush) await segment.pendingTextFlush
    if (!segment.pendingText) return
    const markdown = normalizeMarkdownChunk(segment.streamedText, segment.pendingText)
    segment.pendingText = ''
    segment.streamedText += markdown
  }

  private async flushTextNow(
    state: AgentSessionState,
    segment: Segment,
    opts: { force?: boolean } = {}
  ): Promise<void> {
    raiseStreamError(segment)
    if (
      !opts.force &&
      !safeMarkdownFlush(segment.streamedText + segment.pendingText, segment.streamedText)
    ) {
      this.scheduleTextFlush(state, segment)
      return
    }
    const markdown = normalizeMarkdownChunk(segment.streamedText, segment.pendingText)
    if (!markdown) return
    segment.pendingText = ''
    segment.streamedText += markdown
    await this.streamChunks(state, segment, [...this.planPrefix(state, segment), markdownChunk(markdown)])
  }

  private async flushTask(
    state: AgentSessionState,
    segment: Segment,
    task: StreamTask
  ): Promise<void> {
    const taskChunk = taskUpdateChunk(task)
    const chunks = [...this.planPrefix(state, segment), taskChunk]
    await this.streamChunks(state, segment, chunks)
  }

  private async ensureStream(
    state: AgentSessionState,
    segment: Segment,
    initialChunks: AnyChunk[]
  ): Promise<void> {
    if (segment.streamTs) return
    const response = await this.client.chat.startStream({
      channel: state.channel,
      thread_ts: state.parentTs,
      recipient_team_id: state.recipientTeamId,
      recipient_user_id: state.recipientUserId,
      task_display_mode: 'plan',
      chunks: initialChunks.length ? initialChunks : [markdownChunk(' ')]
    })
    if (!response.ok || !response.ts) throw new Error(response.error ?? 'chat.startStream failed')
    segment.streamTs = response.ts
    await this.clearStatusAfterVisibleOutput(state, initialChunks)
  }

  private async clearStatusAfterVisibleOutput(
    state: AgentSessionState,
    chunks: AnyChunk[]
  ): Promise<void> {
    if (state.statusCleared || !hasVisibleStreamChunks(chunks)) return
    await this.setStatus(state.id, '')
    state.statusCleared = true
  }

  private planPrefix(state: AgentSessionState, segment: Segment): AnyChunk[] {
    const chunks: AnyChunk[] = []
    if (!segment.planStarted) {
      chunks.push(planUpdateChunk(state.title))
      segment.planStarted = true
    }
    return chunks
  }
}

function finalizeOpenTasks(segment: Segment): StreamTask[] {
  const updates: StreamTask[] = []
  for (const [id, task] of segment.tasks) {
    if (task.status !== 'in_progress' && task.status !== 'pending') continue
    const update = {
      ...task,
      status: 'complete' as const,
      output: task.output
    }
    segment.tasks.set(id, update)
    updates.push(update)
  }
  return updates
}

function finalTaskSnapshot(segment: Segment): StreamTask[] {
  return Array.from(segment.tasks.values())
}

function planTitle(title: string, tasks: StreamTask[]): string {
  if (!tasks.length) return title
  const total = tasks.length
  const complete = tasks.filter(task => task.status === 'complete').length
  const failed = tasks.filter(task => task.status === 'error').length
  if (complete + failed === total) return `${title} (${total}/${total})`
  return `${title} (${complete + failed}/${total})`
}

function compactFinalTasks(tasks: StreamTask[]): StreamTask[] {
  const visible: StreamTask[] = tasks.slice(0, FINAL_PLAN_MAX_TASKS).map(task => ({
    ...task,
    title: clipText(task.title, FINAL_PLAN_TITLE_CHARS),
    details: compactTaskBody(task.details, FINAL_PLAN_DETAILS_CHARS),
    output: compactTaskBody(task.output, FINAL_PLAN_OUTPUT_CHARS)
  }))
  const omitted = tasks.length - visible.length
  if (omitted <= 0) return visible
  visible.push({
    id: 'codex-execution-timeline-omitted',
    title: `${omitted} additional command${omitted === 1 ? '' : 's'} omitted from Slack preview`,
    status: 'complete',
    details: richText([
      preformatted(
        'Additional command details were omitted to keep the Slack plan under message size limits.',
        'text'
      )
    ])
  })
  return visible
}

function compactTaskBody(body: StreamTask['details'], maxChars: number): StreamTask['details'] {
  if (!body) return undefined
  if (typeof body === 'string') return clipText(body, maxChars)
  const text = body.elements
    .map(element =>
      element.elements
        .map(inline =>
          'text' in inline ? inline.text : 'url' in inline ? inline.url : 'user_id' in inline ? `<@${inline.user_id}>` : ''
        )
        .join('')
    )
    .filter(Boolean)
    .join('\n')
  const firstPre = body.elements.find(element => element.type === 'rich_text_preformatted')
  const language =
    firstPre?.type === 'rich_text_preformatted' && 'language' in firstPre
      ? String(firstPre.language)
      : 'text'
  return richText([preformatted(clipText(text, maxChars), language)])
}

function clipText(value: string, maxChars: number): string {
  return value.length > maxChars ? `${value.slice(0, maxChars - 13)}\n// truncated` : value
}

function fallbackTextForBlocks(title: string, text: string, footer?: string): string {
  return [title, text, footer].filter(Boolean).join('\n').slice(0, 3900) || title
}

function currentSegment(state: AgentSessionState): Segment {
  return state.segments.at(-1) ?? newSegment()
}

function newSegment(): Segment {
  return {
    id: ulid(),
    textParts: [],
    tasks: new Map(),
    planStarted: false,
    pendingText: '',
    streamedText: '',
    closed: false
  }
}

function requireSession(id: string): AgentSessionState {
  const state = sessions.get(id)
  if (!state) throw new Error('agent_session_not_found')
  return state
}

function raiseStreamError(segment: Segment): void {
  if (segment.streamError) throw segment.streamError
}

function hasVisibleStreamChunks(chunks: AnyChunk[]): boolean {
  return chunks.some(chunk => {
    if (chunk.type === 'markdown_text') return Boolean(chunk.text?.trim())
    if (chunk.type === 'task_update') return Boolean(chunk.title?.trim())
    if (chunk.type === 'plan_update') return Boolean(chunk.title?.trim())
    return false
  })
}

function safeMarkdownFlush(markdown: string, streamedText: string): boolean {
  if (hasOpenFence(markdown)) return false
  if (!streamedText && markdown.trim().length >= FIRST_TEXT_FLUSH_CHARS) return true
  return (
    markdown.endsWith('\n\n') ||
    markdown.endsWith('```') ||
    /[.!?](?:\s|$)$/.test(markdown) ||
    markdown.length >= TEXT_FLUSH_CHARS
  )
}

function balancePendingMarkdown(segment: Segment): void {
  const markdown = segment.streamedText + segment.pendingText
  if (!hasOpenFence(markdown)) return
  segment.pendingText += `${segment.pendingText.endsWith('\n') ? '' : '\n'}\`\`\``
}

function hasOpenFence(markdown: string): boolean {
  const matches = markdown.match(/```/g)
  return Boolean(matches && matches.length % 2 === 1)
}

function normalizeMarkdownChunk(previous: string, chunk: string): string {
  let next = chunk
  if (shouldInsertBoundarySpace(previous, next)) {
    next = ` ${next}`
  }
  next = normalizeFenceBoundaries(previous, next)
  next = next.replace(
    /([^\n])((?:Python|JavaScript|TypeScript|Ruby|Go|Rust|Java|C\+\+|C#|C|PHP|Swift|Kotlin|Shell|Bash|SQL):)/g,
    '$1\n$2'
  )
  return next
}

function normalizeFenceBoundaries(previous: string, markdown: string): string {
  let inFence = hasOpenFence(previous)
  let index = 0
  let out = ''

  while (index < markdown.length) {
    const fenceIndex = markdown.indexOf('```', index)
    if (fenceIndex === -1) {
      out += markdown.slice(index)
      break
    }

    out += markdown.slice(index, fenceIndex)
    const before = out || previous
    if (before && !before.endsWith('\n')) out += '\n'
    out += '```'
    index = fenceIndex + 3

    if (inFence) {
      inFence = false
      if (index < markdown.length && markdown[index] !== '\n') out += '\n'
      continue
    }

    const language = /^[A-Za-z0-9_-]+/.exec(markdown.slice(index))?.[0] ?? ''
    if (language) {
      out += language
      index += language.length
    }
    if (index < markdown.length) {
      if (markdown[index] === '\r' && markdown[index + 1] === '\n') {
        out += '\r\n'
        index += 2
      } else if (markdown[index] === '\n') {
        out += '\n'
        index += 1
      } else {
        out += '\n'
        while (markdown[index] === ' ' || markdown[index] === '\t') index += 1
      }
    }
    inFence = true
  }

  return out
}

function shouldInsertBoundarySpace(previous: string, next: string): boolean {
  if (!previous || !next || /\s$/.test(previous) || /^\s/.test(next)) return false
  if (previous.endsWith('`')) return false
  if (/^[,.;:!?)}\]'"`]/.test(next)) return false
  return /[.!?]$/.test(previous) && /^[A-Za-z0-9]/.test(next)
}

function normalizeDeltaBoundary(previous: string, delta: string): string {
  if (previous && delta && !previous.endsWith('\n') && delta.startsWith('```')) {
    return `\n${delta}`
  }
  return delta
}

function footerBlocks(footer: string): AnyBlock[] {
  return [{ type: 'context', elements: [{ type: 'mrkdwn', text: footer }] }]
}
