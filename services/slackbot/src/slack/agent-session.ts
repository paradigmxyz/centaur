import type { AnyBlock, AnyChunk } from '@slack/types'
import type { WebClient } from '@slack/web-api'
import { ulid } from '@std/ulid'
import { slackReplyLimits } from '../constants'
import { logWarn } from '../logging'
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
import { buildFinalFallbackText, sanitizeFinalMessagePayload } from './final-message'
import {
  markdownToStreamChunks,
  renderMarkdownBlocks,
  shouldShowThinkingBlock,
  thinkingContextBlock
} from './render'
import { clipLines } from './streaming'

type Segment = {
  id: string
  textParts: string[]
  tasks: Map<string, StreamTask>
  streamTs?: string
  streamStartPromise?: Promise<void>
  planStarted: boolean
  headerEmitted: boolean
  pendingText: string
  pendingTextSourceChars: number
  pendingTextPlanPrefix: boolean
  streamedText: string
  streamedTextSourceChars: number
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
  header?: string
  finalCommentaryMarkdown?: string
  finalAnswerMarkdown?: string
  done: boolean
  statusCleared: boolean
  statusUnsupported: boolean
  segments: Segment[]
}

export type OpenAgentSessionInput = {
  channel: string
  parentTs: string
  recipientTeamId: string
  recipientUserId: string
  title?: string
  /**
   * Italic one-line identifier rendered at the top of every assistant message (e.g. "base ·
   * claude-opus-4-7", "legal · codex-gpt-5").
   */
  header?: string
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
  /** When true, stream pending assistant text immediately instead of waiting for heuristics. */
  force?: boolean
  /** When false, stream this text without creating the plan/task surface first. */
  planPrefix?: boolean
}

export type DoneOptions = {
  streamFinalUpdates?: boolean
  commentaryMarkdown?: string
  answerMarkdown?: string
}

function headerMarkdown(header: string): string {
  return `_${header.trim()}_`
}

function headerBlock(header: string): AnyBlock {
  return { type: 'markdown', text: headerMarkdown(header) }
}

const sessions = new Map<string, AgentSessionState>()
const sessionQueues = new Map<string, Promise<void>>()
const THINKING_STATUS = 'Thinking...'
const TEXT_FLUSH_INTERVAL_MS = 250
const TEXT_FLUSH_CHARS = 1000
const FIRST_TEXT_FLUSH_CHARS = 1
const EXECUTION_PLAN_ID = 'codex-execution-timeline'
const LIVE_PLAN_MAX_TASKS = slackReplyLimits.stream.taskCount
const FINAL_PLAN_MAX_TASKS = slackReplyLimits.finalPlan.maxTasks
const FINAL_PLAN_TITLE_CHARS = slackReplyLimits.finalPlan.taskTitleChars
const FINAL_PLAN_DETAILS_LINES = slackReplyLimits.finalPlan.taskDetailsCodeBlockLines
const FINAL_PLAN_OUTPUT_LINES = slackReplyLimits.finalPlan.taskOutputCodeBlockLines
const MAX_LIVE_TEXT_CHARS = slackReplyLimits.stream.maxLiveTextChars

export class AgentSessionRenderer {
  constructor(private readonly client: WebClient) {}

  async open(input: OpenAgentSessionInput): Promise<{ sessionId: string }> {
    const id = ulid()
    const header = input.header?.trim() || undefined
    sessions.set(id, {
      id,
      channel: input.channel,
      parentTs: input.parentTs,
      recipientTeamId: input.recipientTeamId,
      recipientUserId: input.recipientUserId,
      title: input.title ?? 'Execution steps',
      header,
      segments: [newSegment()],
      done: false,
      statusCleared: false,
      statusUnsupported: false
    })
    await this.setStatus(id, THINKING_STATUS)
    return { sessionId: id }
  }

  async text(sessionId: string, markdown: string): Promise<number> {
    const state = requireSession(sessionId)
    const segment = currentSegment(state)

    segment.textParts.push(markdown)
    return await this.queueText(state, segment, markdown)
  }

  async textDelta(sessionId: string, markdownDelta: string, opts: TextOptions = {}): Promise<number> {
    if (!markdownDelta) return 0
    const state = requireSession(sessionId)
    const segment = currentSegment(state)

    const lastIndex = segment.textParts.length - 1
    if (lastIndex >= 0) segment.textParts[lastIndex] += markdownDelta
    else segment.textParts.push(markdownDelta)
    if (opts.flush === false) {
      const accepted = appendPendingTextWithinLiveLimit(segment, markdownDelta)
      return accepted
    }
    return await this.queueText(state, segment, markdownDelta, {
      force: opts.force,
      planPrefix: opts.planPrefix
    })
  }

  streamedTextChars(sessionId: string): number {
    const state = requireSession(sessionId)
    return streamedTextSourceChars(state)
  }

  async blocks(
    sessionId: string,
    blocks: AnyBlock[],
    opts: { planPrefix?: boolean } = {}
  ): Promise<void> {
    if (!blocks.length) return
    const state = requireSession(sessionId)
    const segment = currentSegment(state)
    await this.streamChunks(state, segment, [
      ...(opts.planPrefix === false ? [] : this.planPrefix(state, segment)),
      { type: 'blocks', blocks }
    ])
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

  async done(sessionId: string, opts: DoneOptions = {}): Promise<{ streamedTextChars: number }> {
    const state = requireSession(sessionId)
    state.done = true
    state.finalCommentaryMarkdown = opts.commentaryMarkdown
    state.finalAnswerMarkdown = opts.answerMarkdown
    const streamFinalUpdates = opts.streamFinalUpdates ?? true
    let closed = false
    let streamedTextChars = 0

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
      streamedTextChars = streamedTextSourceChars(state)
      closed = true
    } finally {
      if (!state.statusCleared) {
        state.statusCleared = await this.setStatus(sessionId, '')
      }
      if (closed) sessions.delete(sessionId)
    }
    return { streamedTextChars }
  }

  private async setStatus(sessionId: string, status: string): Promise<boolean> {
    const state = requireSession(sessionId)
    // assistant.threads.setStatus is only valid on AI Assistant threads; cache
    // the first not-supported response so we skip retrying for the session.
    if (state.statusUnsupported) return false
    try {
      const response = await this.client.assistant.threads.setStatus({
        channel_id: state.channel,
        thread_ts: state.parentTs,
        status,
        ...(status ? { loading_messages: [status] } : {})
      })
      if (!response.ok) {
        const errorCode = response.error ?? 'unknown_error'
        if (isUnsupportedStatusChannel(errorCode)) {
          state.statusUnsupported = true
          logStatusUnsupported(state, errorCode)
          return false
        }
        logStatusFailure(state, status, errorCode)
        return false
      }
      return true
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      if (isUnsupportedStatusChannel(message)) {
        state.statusUnsupported = true
        logStatusUnsupported(state, message)
        return false
      }
      logStatusFailure(state, status, message)
      return false
    }
  }

  private async closeTextStream(state: AgentSessionState, segment: Segment): Promise<void> {
    raiseStreamError(segment)
    if (segment.closed) return
    const hasFinalText = Boolean(
      state.finalCommentaryMarkdown?.trim() || state.finalAnswerMarkdown?.trim()
    )
    if (!segment.streamTs && !segment.textParts.length && !segment.tasks.size && !hasFinalText) {
      return
    }
    await this.ensureStream(state, segment, [])
    if (!segment.streamTs) return
    const originalTasks = finalTaskSnapshot(segment)
    const tasks = compactFinalTasks(originalTasks)
    const commentaryMarkdown = state.finalCommentaryMarkdown?.trim() ?? ''
    const answerSource =
      state.finalAnswerMarkdown?.trim() || segment.streamedText.trim() || segment.textParts.join('')
    const answerMarkdown = finalMarkdownForBlocks(answerSource)
    const streamedTextLive =
      Boolean(segment.streamedText.trim()) && segment.streamedText.length < MAX_LIVE_TEXT_CHARS
    const showThinking =
      !streamedTextLive && shouldShowThinkingBlock(commentaryMarkdown, answerMarkdown)
    const thinkingBlock = showThinking ? thinkingContextBlock(commentaryMarkdown) : null
    // Slack accumulates appendStream chunks; stopStream blocks are the composed final layout.
    // Only add blocks for content that was not streamed live; live task_update chunks carry
    // fenced details/output, and the header has already been streamed as the first chunk.
    const blocks = sanitizeFinalMessagePayload([
      ...(tasks.length && !segment.planStarted
        ? [planBlock(planTitle(state.title, originalTasks), tasks, EXECUTION_PLAN_ID)]
        : []),
      ...(thinkingBlock ? [thinkingBlock] : []),
      ...(!streamedTextLive && answerMarkdown ? renderMarkdownBlocks(answerMarkdown) : [])
    ] as AnyBlock[])
    const fallbackText = buildFinalFallbackText({
      title: state.title,
      commentaryMarkdown: showThinking ? commentaryMarkdown : '',
      answerMarkdown
    })
    const stopResponse = await this.client.chat.stopStream({
      channel: state.channel,
      ts: segment.streamTs,
      chunks: markdownToStreamChunks(blocks.length || streamedTextLive ? ' ' : fallbackText),
      ...(blocks.length ? { blocks } : {})
    })
    if (!stopResponse.ok) throw new Error(stopResponse.error ?? 'chat.stopStream failed')
    segment.closed = true
  }

  private async streamChunks(
    state: AgentSessionState,
    segment: Segment,
    chunks: AnyChunk[]
  ): Promise<void> {
    raiseStreamError(segment)
    if (!chunks.length || segment.closed) return
    const effectiveChunks = this.withHeaderPrefix(state, segment, chunks)
    if (!segment.streamTs) {
      const initialChunksUsed = await this.ensureStream(state, segment, effectiveChunks)
      if (initialChunksUsed) return
    }
    if (!segment.streamTs) throw new Error('chat.startStream did not return a stream ts')
    const response = await this.client.chat.appendStream({
      channel: state.channel,
      ts: segment.streamTs,
      chunks: effectiveChunks
    })
    if (!response.ok) throw new Error(response.error ?? 'chat.appendStream failed')
    await this.clearStatusAfterVisibleOutput(state, effectiveChunks)
  }

  private async queueText(
    state: AgentSessionState,
    segment: Segment,
    markdown: string,
    opts: { force?: boolean; planPrefix?: boolean } = {}
  ): Promise<number> {
    raiseStreamError(segment)
    const planPrefix = opts.planPrefix !== false
    if (segment.pendingText && segment.pendingTextPlanPrefix !== planPrefix) {
      await this.flushText(state, segment, { force: true })
    }
    segment.pendingTextPlanPrefix = planPrefix
    const accepted = appendPendingTextWithinLiveLimit(segment, markdown)
    if (accepted <= 0) return 0
    if (opts.force) {
      await this.flushText(state, segment, { force: true })
      return accepted
    }
    if (segment.pendingText.length >= TEXT_FLUSH_CHARS) {
      await this.flushText(state, segment, { force: false })
      return accepted
    }
    this.scheduleTextFlush(state, segment)
    return accepted
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
    segment.pendingTextSourceChars = 0
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
      !safeMarkdownFlush(
        segment.streamedText + segment.pendingText,
        segment.pendingText,
        segment.streamedText
      )
    ) {
      this.scheduleTextFlush(state, segment)
      return
    }
    const markdown = normalizeMarkdownChunk(segment.streamedText, segment.pendingText)
    if (!markdown) return
    const pendingSourceChars = segment.pendingTextSourceChars
    segment.pendingText = ''
    segment.pendingTextSourceChars = 0
    await this.streamChunks(state, segment, [
      ...(segment.pendingTextPlanPrefix ? this.planPrefix(state, segment) : []),
      ...markdownToStreamChunks(markdown)
    ])
    segment.streamedText += markdown
    segment.streamedTextSourceChars += pendingSourceChars
  }

  private async flushTask(
    state: AgentSessionState,
    segment: Segment,
    task: StreamTask
  ): Promise<void> {
    if (!shouldStreamTaskUpdate(segment, task.id)) return
    const taskChunk = taskUpdateChunk(task)
    const chunks = [...this.planPrefix(state, segment), taskChunk]
    await this.streamChunks(state, segment, chunks)
  }

  private async ensureStream(
    state: AgentSessionState,
    segment: Segment,
    initialChunks: AnyChunk[]
  ): Promise<boolean> {
    if (segment.streamTs) return false
    if (segment.streamStartPromise) {
      await segment.streamStartPromise
      return false
    }

    const chunks = initialChunks.length ? initialChunks : [markdownChunk(' ')]
    segment.streamStartPromise = (async () => {
      const response = await this.client.chat.startStream({
        channel: state.channel,
        thread_ts: state.parentTs,
        recipient_team_id: state.recipientTeamId,
        recipient_user_id: state.recipientUserId,
        task_display_mode: 'plan',
        chunks
      })
      if (!response.ok || !response.ts) throw new Error(response.error ?? 'chat.startStream failed')
      segment.streamTs = response.ts
      await this.clearStatusAfterVisibleOutput(state, chunks)
    })()

    try {
      await segment.streamStartPromise
      return initialChunks.length > 0
    } finally {
      segment.streamStartPromise = undefined
    }
  }

  private async clearStatusAfterVisibleOutput(
    state: AgentSessionState,
    chunks: AnyChunk[]
  ): Promise<void> {
    if (state.statusCleared || !hasVisibleStreamChunks(chunks)) return
    state.statusCleared = await this.setStatus(state.id, '')
  }

  private planPrefix(state: AgentSessionState, segment: Segment): AnyChunk[] {
    const chunks: AnyChunk[] = []
    if (!segment.planStarted) {
      chunks.push(planUpdateChunk(state.title))
      segment.planStarted = true
    }
    return chunks
  }

  private withHeaderPrefix(
    state: AgentSessionState,
    segment: Segment,
    chunks: AnyChunk[]
  ): AnyChunk[] {
    const header = state.header?.trim()
    if (!header || segment.headerEmitted) return chunks
    segment.headerEmitted = true
    return [markdownChunk(`${headerMarkdown(header)}\n`), ...chunks]
  }
}

function logStatusFailure(state: AgentSessionState, status: string, error: string): void {
  logWarn('slack_assistant_status_failed', {
    channel_id: state.channel,
    thread_ts: state.parentTs,
    status: status ? 'set' : 'clear',
    error
  })
}

const UNSUPPORTED_STATUS_ERROR_CODES = new Set([
  'user_not_found',
  'channel_not_found',
  'channel_type_not_supported',
  'not_in_channel',
  'restricted_action'
])

function isUnsupportedStatusChannel(error: string): boolean {
  const trimmed = error.trim().toLowerCase()
  if (UNSUPPORTED_STATUS_ERROR_CODES.has(trimmed)) return true
  for (const code of UNSUPPORTED_STATUS_ERROR_CODES) {
    if (trimmed.includes(code)) return true
  }
  return false
}

function logStatusUnsupported(state: AgentSessionState, error: string): void {
  logWarn('slack_assistant_status_unsupported', {
    channel_id: state.channel,
    thread_ts: state.parentTs,
    error
  })
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

function shouldStreamTaskUpdate(segment: Segment, taskId: string): boolean {
  return Array.from(segment.tasks.keys()).indexOf(taskId) < LIVE_PLAN_MAX_TASKS
}

function planTitle(title: string, tasks: StreamTask[]): string {
  if (!tasks.length) return title
  const total = tasks.length
  const finished = tasks.filter(
    task => task.status === 'complete' || task.status === 'error'
  ).length
  if (finished === total) return `${title} (${total}/${total})`
  return `${title} (${finished}/${total})`
}

function compactFinalTasks(tasks: StreamTask[]): StreamTask[] {
  const visible: StreamTask[] = tasks.slice(0, FINAL_PLAN_MAX_TASKS).map(task => ({
    ...task,
    title: clipText(task.title, FINAL_PLAN_TITLE_CHARS),
    details: compactTaskBody(task.details, FINAL_PLAN_DETAILS_LINES),
    output: compactTaskBody(task.output, FINAL_PLAN_OUTPUT_LINES)
  }))
  const omitted = tasks.length - visible.length
  if (omitted <= 0) return visible
  visible.push({
    id: 'codex-execution-timeline-omitted',
    title: `${omitted} more command${omitted === 1 ? '' : 's'} not shown in preview (${visible.length} of ${tasks.length})`,
    status: 'complete',
    details: richText([
      preformatted(
        `This reply ran ${tasks.length} commands. The Slack plan preview shows the first ${visible.length}; raise slackReplyLimits.finalPlan.maxTasks if you need more visible rows.`,
        'text'
      )
    ])
  })
  return visible
}

function compactTaskBody(body: StreamTask['details'], maxLines: number): StreamTask['details'] {
  if (!body) return undefined
  if (typeof body === 'string') return clipLines(body, maxLines)
  const text = body.elements
    .map(element =>
      element.elements
        .map(inline =>
          'text' in inline
            ? inline.text
            : 'url' in inline
              ? inline.url
              : 'user_id' in inline
                ? `<@${inline.user_id}>`
                : ''
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
  return richText([preformatted(clipLines(text, maxLines), language)])
}

function finalMarkdownForBlocks(markdown: string): string {
  return clipText(markdown, slackReplyLimits.mixedBodyAndPlan.maxVisibleChars)
}

function clipText(value: string, maxChars: number): string {
  if (maxChars <= 0) return ''
  if (maxChars <= 13) return value.length > maxChars ? value.slice(0, maxChars) : value
  return value.length > maxChars ? `${value.slice(0, maxChars - 13)}\n// truncated` : value
}

function currentSegment(state: AgentSessionState): Segment {
  return state.segments.at(-1) ?? newSegment()
}

function streamedTextSourceChars(state: AgentSessionState): number {
  return state.segments.reduce((total, segment) => total + segment.streamedTextSourceChars, 0)
}

function newSegment(): Segment {
  return {
    id: ulid(),
    textParts: [],
    tasks: new Map(),
    planStarted: false,
    headerEmitted: false,
    pendingText: '',
    pendingTextSourceChars: 0,
    pendingTextPlanPrefix: true,
    streamedText: '',
    streamedTextSourceChars: 0,
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

function safeMarkdownFlush(markdown: string, pendingText: string, streamedText: string): boolean {
  if (hasOpenFence(markdown)) return false
  if (!streamedText && pendingText.trim().length < FIRST_TEXT_FLUSH_CHARS) return false
  if (isBareListPrefix(pendingText)) return false
  return (
    pendingText.endsWith('\n') ||
    pendingText.endsWith('\n\n') ||
    pendingText.endsWith('```') ||
    /[.!?](?:\s|$)$/.test(pendingText)
  )
}

function isBareListPrefix(markdown: string): boolean {
  return /^\s*\d+[.)]\s*$/.test(markdown)
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

function appendPendingTextWithinLiveLimit(segment: Segment, markdown: string): number {
  const previous = segment.streamedText + segment.pendingText
  const normalized = normalizeDeltaBoundary(previous, markdown)
  const prefixChars = normalized.length - markdown.length
  const remaining = MAX_LIVE_TEXT_CHARS - segment.streamedText.length - segment.pendingText.length
  if (remaining <= 0) return 0
  const acceptedNormalized = normalized.slice(0, remaining)
  const acceptedSourceChars = Math.max(0, acceptedNormalized.length - prefixChars)
  segment.pendingText += acceptedNormalized
  segment.pendingTextSourceChars += acceptedSourceChars
  return acceptedSourceChars
}

export async function withAgentSessionLock<T>(sessionId: string, fn: () => Promise<T>): Promise<T> {
  const previous = sessionQueues.get(sessionId) ?? Promise.resolve()
  const run = previous.catch(() => undefined).then(fn)
  const cleanup = run.then(
    () => undefined,
    () => undefined
  )
  sessionQueues.set(sessionId, cleanup)
  void cleanup.finally(() => {
    if (sessionQueues.get(sessionId) === cleanup) sessionQueues.delete(sessionId)
  })
  return run
}
