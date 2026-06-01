import type { WebClient } from '@slack/web-api'
import {
  CodexAppServerRendererEventMapper,
  isTerminalCodexAppServerEvent,
  rustSessionEventToServerNotification,
  type RendererEvent
} from '@centaur/rendering'
import { logInfo } from '../logging'
import { SlackChatSDKRenderer } from '../rendering/slack'

type ActiveCodexSessionState = {
  mapper: CodexAppServerRendererEventMapper
  threadId: string
  streamedAnswerChars: number
  done: boolean
}

type CompletedCodexSessionState = {
  threadId: string
  streamedAnswerChars: number
  completedAt: number
}

const states = new Map<string, ActiveCodexSessionState>()
const completedStates = new Map<string, CompletedCodexSessionState>()
const COMPLETED_STATE_TTL_MS = 10 * 60 * 1000

export class CodexSessionRenderer {
  private readonly renderer: SlackChatSDKRenderer

  constructor(client: WebClient) {
    this.renderer = new SlackChatSDKRenderer(client)
  }

  async event(
    agentSessionId: string,
    event: unknown
  ): Promise<{ threadId?: string; done: boolean; streamedAnswerChars: number }> {
    const completed = completedState(agentSessionId)
    if (completed) {
      if (isTerminalSourceEvent(event)) {
        logCodexTerminalEventIgnoredAfterDone(agentSessionId, event, completed)
      }
      return {
        threadId: completed.threadId || undefined,
        done: true,
        streamedAnswerChars: completed.streamedAnswerChars
      }
    }

    const state = getState(agentSessionId)
    const rendererEvents = state.mapper.process(event)
    await this.consumeRendererEvents(agentSessionId, state, rendererEvents)
    if (state.mapper.threadId()) state.threadId = state.mapper.threadId()
    if (state.done || state.mapper.isDone()) {
      state.done = true
      completedStates.set(agentSessionId, {
        threadId: state.threadId,
        streamedAnswerChars: state.streamedAnswerChars,
        completedAt: Date.now()
      })
      states.delete(agentSessionId)
    }

    return {
      threadId: state.threadId || undefined,
      done: state.done,
      streamedAnswerChars: state.streamedAnswerChars
    }
  }

  async done(agentSessionId: string, threadId?: string): Promise<void> {
    const state = getState(agentSessionId)
    if (state.done) return
    if (threadId) state.threadId = threadId
    await this.consumeRendererEvents(agentSessionId, state, state.mapper.flush())
    state.done = true
    completedStates.set(agentSessionId, {
      threadId: state.threadId,
      streamedAnswerChars: state.streamedAnswerChars,
      completedAt: Date.now()
    })
    states.delete(agentSessionId)
  }

  private async consumeRendererEvents(
    agentSessionId: string,
    state: ActiveCodexSessionState,
    events: RendererEvent[]
  ): Promise<void> {
    for (const event of events) {
      if (event.type === 'renderer.task.update') {
        await this.renderer.render(agentSessionId, event)
      } else if (event.type === 'renderer.message.delta') {
        const result = await this.renderer.render(agentSessionId, event)
        if (result.streamedTextChars !== undefined) state.streamedAnswerChars = result.streamedTextChars
      } else if (event.type === 'renderer.title.update') {
        await this.renderer.render(agentSessionId, event)
      } else if (event.type === 'renderer.done') {
        if (event.threadId) state.threadId = event.threadId
        const result = await this.renderer.render(agentSessionId, event)
        if (result.streamedTextChars !== undefined) state.streamedAnswerChars = result.streamedTextChars
        if (result.closed) state.done = true
      }
    }
  }
}

export function hasActiveCodexSession(agentSessionId: string): boolean {
  const state = states.get(agentSessionId)
  return Boolean(state && !state.done)
}

function getState(agentSessionId: string): ActiveCodexSessionState {
  let state = states.get(agentSessionId)
  if (!state) {
    state = {
      mapper: new CodexAppServerRendererEventMapper({
        sessionId: agentSessionId,
        logInfo: slackCodexLogInfo
      }),
      threadId: '',
      streamedAnswerChars: 0,
      done: false
    }
    states.set(agentSessionId, state)
  }
  return state
}

const slackLogEventNames: Record<string, string> = {
  codex_renderer_canonical_answer_correction: 'slack_codex_canonical_answer_correction',
  codex_renderer_item_completed_missing_id: 'slack_codex_item_completed_missing_id',
  codex_renderer_terminal_event_received: 'slack_codex_terminal_event_received',
  codex_renderer_unphased_final_agent_message_classified:
    'slack_codex_unphased_final_agent_message_classified'
}

function slackCodexLogInfo(event: string, fields: Record<string, unknown>): void {
  logInfo(slackLogEventNames[event] ?? event, fields)
}

function completedState(agentSessionId: string): CompletedCodexSessionState | undefined {
  const completed = completedStates.get(agentSessionId)
  if (!completed) return undefined
  if (Date.now() - completed.completedAt > COMPLETED_STATE_TTL_MS) {
    completedStates.delete(agentSessionId)
    return undefined
  }
  return completed
}

function isTerminalSourceEvent(event: unknown): boolean {
  if (isTerminalCodexAppServerEvent(event)) return true
  const rustMapped = rustSessionEventToServerNotification(event)
  return rustMapped?.kind === 'completed' || rustMapped?.kind === 'failed'
}

function logCodexTerminalEventIgnoredAfterDone(
  agentSessionId: string,
  event: unknown,
  completed: CompletedCodexSessionState
): void {
  logInfo('slack_codex_terminal_event_ignored_after_done', {
    agent_session_id: agentSessionId,
    centaur_thread_key: recordValue(event, 'centaur_thread_key'),
    execution_id: recordValue(event, 'centaur_execution_id'),
    assignment_generation: recordValue(event, 'centaur_assignment_generation'),
    event_type: recordValue(event, 'type') ?? recordValue(event, 'eventKind') ?? recordValue(event, 'event'),
    codex_session_id:
      completed.threadId || recordValue(event, 'session_id') || recordValue(event, 'thread_id'),
    already_completed: true,
    will_close: false,
    result_text_chars: terminalResultText(event).length,
    streamed_answer_chars_at_completion: completed.streamedAnswerChars,
    completed_age_ms: Date.now() - completed.completedAt
  })
}

function terminalResultText(event: unknown): string {
  if (!event || typeof event !== 'object') return ''
  const record = event as Record<string, unknown>
  for (const key of ['result', 'result_text', 'text', 'final_text']) {
    const value = record[key]
    if (typeof value !== 'string') continue
    const text = value.trim()
    if (text) return text
  }
  return ''
}

function recordValue(event: unknown, key: string): unknown {
  if (!event || typeof event !== 'object') return undefined
  return (event as Record<string, unknown>)[key]
}
