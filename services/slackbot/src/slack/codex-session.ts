import type { WebClient } from '@slack/web-api'
import { AgentSessionRenderer } from './agent-session'
import { preformatted as pre, section, text, type StreamRichTextElement } from './streaming'

type HarnessTask = {
  id: string
  title: string
  status: 'pending' | 'in_progress' | 'complete' | 'error'
  details: StreamRichTextElement[]
  output: StreamRichTextElement[]
}

type CodexSessionState = {
  threadId: string
  stepCounter: number
  messageText: string
  planText: string
  taskByUseId: Map<string, HarnessTask>
  commandOutputById: Map<string, string>
  done: boolean
}

const states = new Map<string, CodexSessionState>()

export class CodexSessionRenderer {
  private readonly renderer: AgentSessionRenderer

  constructor(client: WebClient) {
    this.renderer = new AgentSessionRenderer(client)
  }

  async event(agentSessionId: string, event: any): Promise<{ threadId?: string; done: boolean }> {
    const state = getState(agentSessionId)
    if (event?.session_id) state.threadId = String(event.session_id)
    if (event?.thread_id) state.threadId = String(event.thread_id)

    const structuredPlan = structuredPlanUpdate(event)
    if (structuredPlan) {
      await this.publishStructuredPlan(agentSessionId, structuredPlan)
    }

    const planText = planTextUpdate(event)
    if (planText) {
      state.planText = event?.type === 'item.plan.delta' ? state.planText + planText : planText
      await this.publishPlanText(agentSessionId, state.planText)
    }

    const command = commandExecution(event)
    if (command) {
      const id = commandId(command)
      const aggregatedOutput = commandAggregatedOutput(command)
      if (aggregatedOutput) state.commandOutputById.set(id, aggregatedOutput)
      const existing = state.taskByUseId.get(id)
      const task = commandTask(command, event?.type, existing, state.commandOutputById.get(id))
      const merged = mergeTask(existing, task)
      state.taskByUseId.set(merged.id, merged)
      await this.publishTask(agentSessionId, {
        ...merged,
        details: task.details,
        output: task.output
      })
    }

    const fileChange = fileChangeEvent(event)
    if (fileChange) {
      const existing = state.taskByUseId.get(fileChangeId(fileChange))
      const task = fileChangeTask(fileChange, event?.type, existing)
      const merged = mergeTask(existing, task)
      state.taskByUseId.set(merged.id, merged)
      await this.publishTask(agentSessionId, merged)
    }

    const outputDelta = commandOutputDelta(event)
    if (outputDelta) {
      const current = state.commandOutputById.get(outputDelta.id) ?? ''
      const output = current + outputDelta.delta
      state.commandOutputById.set(outputDelta.id, output)
      const task = state.taskByUseId.get(outputDelta.id) ?? {
        id: outputDelta.id,
        title: 'Run command',
        status: 'in_progress',
        details: [],
        output: []
      }
      const updated = {
        ...task,
        output: commandOutputElements(output)
      }
      state.taskByUseId.set(outputDelta.id, updated)
      await this.publishTask(agentSessionId, { ...updated, details: [], output: updated.output })
    }

    for (const tool of toolUses(event)) {
      const task: HarnessTask = {
        id: `task-${++state.stepCounter}`,
        title: titleFor(tool),
        status: 'in_progress',
        details: detailElementsForTool(tool),
        output: []
      }
      state.taskByUseId.set(String(tool.id), task)
      await this.publishTask(agentSessionId, task)
    }

    for (const result of toolResults(event)) {
      const toolUseId = String(result.tool_use_id ?? '')
      const task = state.taskByUseId.get(toolUseId) ?? {
        id: `task-${++state.stepCounter}`,
        title: 'Tool result',
        status: 'in_progress',
        details: [],
        output: []
      }
      state.taskByUseId.set(toolUseId || task.id, task)
      task.status = result.is_error ? 'error' : 'complete'
      task.output = outputElementsForResult(result)
      await this.publishTask(agentSessionId, { ...task, details: [], output: task.output })
    }

    const assistantMessage = assistantText(event)
    if (assistantMessage) {
      const delta = messageDelta(state.messageText, assistantMessage)
      if (delta) {
        state.messageText += delta
        await this.renderer.textDelta(agentSessionId, delta)
      }
    }

    const reasoningMessage = reasoningText(event).trim()
    if (reasoningMessage) {
      const task: HarnessTask = {
        id: `reasoning-${++state.stepCounter}`,
        title: 'Reasoning',
        status: 'complete',
        details: [section([text(reasoningMessage)])],
        output: []
      }
      await this.publishTask(agentSessionId, task)
    }

    if (event?.type === 'result' || event?.type === 'turn.done') {
      if (typeof event.result === 'string' && !state.messageText.trim()) {
        const resultText = event.result.trim()
        if (resultText) {
          state.messageText += resultText
          await this.renderer.text(agentSessionId, resultText)
        }
      }
      await this.done(agentSessionId)
    }

    return { threadId: state.threadId || undefined, done: state.done }
  }

  async done(agentSessionId: string): Promise<void> {
    const state = getState(agentSessionId)
    if (state.done) return
    await this.renderer.done(
      agentSessionId,
      state.threadId ? codexFooter(state.threadId) : undefined
    )
    state.done = true
    states.delete(agentSessionId)
  }

  private async publishTask(agentSessionId: string, task: HarnessTask): Promise<void> {
    await this.renderer.step(agentSessionId, {
      id: task.id,
      title: task.title,
      status: task.status,
      details: elementsToMarkdown(task.details),
      output: elementsToMarkdown(task.output)
    })
  }

  private async publishStructuredPlan(
    agentSessionId: string,
    plan: Array<{ step: string; status?: string }>
  ): Promise<void> {
    for (const [index, item] of plan.entries()) {
      await publishPlanTask(
        this.renderer,
        agentSessionId,
        index,
        String(item.step ?? ''),
        planStatus(item.status)
      )
    }
  }

  private async publishPlanText(agentSessionId: string, value: string): Promise<void> {
    const steps = parsePlanText(value)
    if (!steps.length) return
    for (const [index, item] of steps.entries()) {
      await publishPlanTask(this.renderer, agentSessionId, index, item.step, item.status)
    }
  }
}

export function codexFooter(threadId: string): string {
  return `Codex thread \`${threadId}\``
}

function getState(agentSessionId: string): CodexSessionState {
  let state = states.get(agentSessionId)
  if (!state) {
    state = {
      threadId: '',
      stepCounter: 0,
      messageText: '',
      planText: '',
      taskByUseId: new Map(),
      commandOutputById: new Map(),
      done: false
    }
    states.set(agentSessionId, state)
  }
  return state
}

function content(event: any): any[] {
  return Array.isArray(event?.message?.content) ? event.message.content : []
}

function assistantText(event: any): string {
  if (event?.type === 'item.agentMessage.delta') {
    const delta = event.delta ?? event.text ?? event.content ?? ''
    if (delta && typeof delta === 'object') {
      return String(delta.text ?? delta.content ?? '')
    }
    return String(delta)
  }
  if (
    event?.type === 'item.completed' &&
    (event?.item?.type === 'agentMessage' || event?.item?.type === 'agent_message')
  ) {
    return String(event.item.text ?? '')
  }
  if (event?.type !== 'assistant') return ''
  return content(event)
    .map(part => (part?.type === 'text' ? (part.text ?? '') : ''))
    .filter(Boolean)
    .join('')
}

function messageDelta(current: string, incoming: string): string {
  if (!current) return incoming
  if (incoming.startsWith(current)) return incoming.slice(current.length)
  if (current.endsWith(incoming)) return ''
  return incoming
}

function reasoningText(event: any): string {
  if (event?.type !== 'reasoning') return ''
  return String(event.text ?? event.thinking ?? '')
}

function toolUses(event: any): any[] {
  if (event?.type !== 'assistant') return []
  return content(event).filter(part => part?.type === 'tool_use')
}

function toolResults(event: any): any[] {
  if (event?.type !== 'user' && event?.type !== 'tool') return []
  const direct = Array.isArray(event?.content) ? event.content : []
  return direct.filter((part: any) => part?.type === 'tool_result' || part?.tool_use_id)
}

function commandExecution(event: any): Record<string, any> | null {
  if (event?.type === 'command_execution') return event
  if (
    event?.type !== 'item.started' &&
    event?.type !== 'item.updated' &&
    event?.type !== 'item.completed'
  )
    return null
  const item = event.item
  if (!item || (item.type !== 'commandExecution' && item.type !== 'command_execution')) return null
  return item
}

function fileChangeEvent(event: any): Record<string, any> | null {
  if (event?.type === 'file_change') return event
  if (
    event?.type !== 'item.started' &&
    event?.type !== 'item.updated' &&
    event?.type !== 'item.completed'
  )
    return null
  const item = event.item
  if (!item || (item.type !== 'fileChange' && item.type !== 'file_change')) return null
  return item
}

function structuredPlanUpdate(event: any): Array<{ step: string; status?: string }> | null {
  if (event?.type !== 'turn.plan.updated') return null
  return Array.isArray(event.plan) ? event.plan : null
}

function planTextUpdate(event: any): string {
  if (event?.type === 'item.plan.delta') {
    return String(event.delta ?? event.text ?? '')
  }
  if (event?.type === 'item.completed' && event?.item?.type === 'plan') {
    return String(event.item.text ?? '')
  }
  return ''
}

function parsePlanText(value: string): Array<{ step: string; status: HarnessTask['status'] }> {
  return value
    .split('\n')
    .map(line => {
      const trimmed = line.trim()
      if (!/^[-*]\s+|\d+[.)]\s+/.test(trimmed)) return null
      return {
        step: trimmed,
        status: /\[[xX]\]/.test(trimmed) ? ('complete' as const) : ('pending' as const)
      }
    })
    .filter(item => item !== null)
}

function planStatus(value: string | undefined): HarnessTask['status'] {
  const status = String(value ?? '').toLowerCase()
  if (status === 'inprogress' || status === 'in_progress' || status === 'running')
    return 'in_progress'
  if (status === 'completed' || status === 'complete' || status === 'done') return 'complete'
  if (status === 'failed' || status === 'error') return 'error'
  return 'pending'
}

function stripPlanMarker(value: string): string {
  return value
    .replace(/^\s*(?:[-*]|\d+[.)])\s+/, '')
    .replace(/^\[[ xX]\]\s+/, '')
    .trim()
}

async function publishPlanTask(
  renderer: AgentSessionRenderer,
  agentSessionId: string,
  index: number,
  step: string,
  status: HarnessTask['status']
): Promise<void> {
  const title = oneLine(stripPlanMarker(step), 256)
  if (!title) return
  await renderer.step(agentSessionId, {
    id: `plan-${index + 1}`,
    title,
    status
  })
}

function commandOutputDelta(event: any): { id: string; delta: string } | null {
  if (event?.type !== 'item.commandExecution.outputDelta') return null
  const id = String(event.itemId ?? event.item_id ?? '')
  const delta = String(event.delta ?? '')
  return id && delta ? { id, delta } : null
}

function commandId(item: any): string {
  return String(item.id ?? item.itemId ?? item.command_id ?? item.command ?? 'command')
}

function fileChangeId(item: any): string {
  return String(item.id ?? item.itemId ?? item.path ?? 'file-change')
}

function commandTask(
  item: any,
  eventType: string,
  existing?: HarnessTask,
  accumulatedOutput?: string
): HarnessTask {
  const id = commandId(item)
  const command = String(item.command ?? 'Command')
  const status = commandStatus(item, eventType)
  const exitCode = item.exitCode ?? item.exit_code
  const isCompletionUpdate =
    eventType === 'item.completed' || status === 'complete' || status === 'error'
  const output = commandOutputElements(accumulatedOutput ?? '', exitCode)
  return {
    id,
    title: command === 'Command' ? 'Run command' : `Run command: ${oneLine(command, 220)}`,
    status,
    details: isCompletionUpdate && existing ? [] : [pre(command, 'bash')],
    output
  }
}

function commandAggregatedOutput(item: any): string {
  for (const key of ['aggregated_output', 'aggregatedOutput', 'output', 'stdout', 'stderr']) {
    const value = item?.[key]
    if (typeof value === 'string' && value) return value
  }
  return ''
}

function commandOutputElements(
  output: string,
  exitCode?: number | string | null
): StreamRichTextElement[] {
  const elements: StreamRichTextElement[] = []
  if (output) {
    const formatted = formatCommandOutput(output)
    elements.push(pre(formatted.body, formatted.language))
  }
  if (exitCode !== null && exitCode !== undefined) {
    elements.push(section([text(`exit code ${exitCode}`)]))
  }
  return elements
}

function formatCommandOutput(output: string): { body: string; language: string } {
  const trimmed = output.trim()
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    try {
      return {
        body: clipJsonPreview(JSON.stringify(JSON.parse(trimmed), null, 2)),
        language: 'json'
      }
    } catch {}
  }
  return { body: clip(output), language: languageFromContent(output) }
}

function clipJsonPreview(value: string, max = 420): string {
  return value.length > max ? `${value.slice(0, max).trimEnd()}\n// truncated` : value
}

function fileChangeTask(item: any, eventType: string, existing?: HarnessTask): HarnessTask {
  const id = fileChangeId(item)
  const changes = Array.isArray(item.changes) ? item.changes : []
  const paths = changes.map((change: any) => String(change.path ?? '')).filter(Boolean)
  const uniquePaths: string[] = Array.from(new Set(paths))
  const diff = changes
    .map((change: any) => String(change.diff ?? change.unified_diff ?? '').trim())
    .filter(Boolean)
    .join('\n\n')
  return {
    id,
    title:
      uniquePaths.length === 1
        ? `Edit ${uniquePaths[0]}`
        : uniquePaths.length > 1
          ? `Edit ${uniquePaths.length} files`
          : 'Apply file changes',
    status: itemStatus(item, eventType),
    details: uniquePaths.length
      ? [section([text('Files: '), text(uniquePaths.join(', '), { code: true })])]
      : (existing?.details ?? []),
    output: diff ? [pre(clip(diff), 'diff')] : (existing?.output ?? [])
  }
}

function mergeTask(existing: HarnessTask | undefined, update: HarnessTask): HarnessTask {
  return {
    ...update,
    details: update.details.length ? update.details : (existing?.details ?? []),
    output: update.output.length ? update.output : (existing?.output ?? [])
  }
}

function commandStatus(item: any, eventType: string): HarnessTask['status'] {
  return itemStatus(item, eventType, item.exitCode ?? item.exit_code)
}

function itemStatus(item: any, eventType: string, exitCode?: number | null): HarnessTask['status'] {
  const status = String(item.status ?? '').toLowerCase()
  if (status === 'failed' || status === 'declined') return 'error'
  if (status === 'completed' || eventType === 'item.completed') {
    return 'complete'
  }
  return 'in_progress'
}

function elementsToMarkdown(elements: StreamRichTextElement[]): string {
  return elements.map(elementToMarkdown).filter(Boolean).join('\n\n')
}

function elementToMarkdown(element: StreamRichTextElement): string {
  if (element.type === 'rich_text_preformatted') {
    const body = element.elements?.map(inline => inline.text ?? '').join('') ?? ''
    return `\`\`\`${element.language ?? ''}\n${body}\n\`\`\``
  }
  if (element.type === 'rich_text_section') {
    return (element.elements ?? [])
      .map(inline => {
        if ('url' in inline) return inline.text ? `[${inline.text}](${inline.url})` : inline.url
        if ('user_id' in inline) return `<@${inline.user_id}>`
        const value = inline.text ?? ''
        return inline.style?.code ? `\`${value}\`` : value
      })
      .join('')
  }
  return ''
}

function titleFor(tool: any): string {
  if (tool.name === 'Bash') {
    const command = stringInput(tool.input, 'cmd')
    return command ? `Run command: ${oneLine(command, 220)}` : 'Run command'
  }
  if (tool.name === 'create_file') return 'Create file'
  if (tool.name === 'edit_file') return 'Edit file'
  return `Use ${tool.name ?? 'tool'}`
}

function detailElementsForTool(tool: any): StreamRichTextElement[] {
  if (tool.name === 'Bash') return [pre(stringInput(tool.input, 'cmd'), 'bash')]
  if (tool.name === 'create_file') {
    const path = stringInput(tool.input, 'path', 'file')
    return [
      section([text('Created '), text(path, { code: true })]),
      pre(stringInput(tool.input, 'content'), languageFromPath(path))
    ]
  }
  if (tool.name === 'edit_file') {
    const path = stringInput(tool.input, 'path', 'file')
    const newStr = stringInput(tool.input, 'new_str')
    const diff = stringInput(tool.input, 'diff')
    const fileContent = stringInput(tool.input, 'content')
    if (newStr)
      return [
        section([text('Edited '), text(path, { code: true })]),
        pre(newStr, languageFromPath(path))
      ]
    if (diff)
      return [section([text('Edited '), text(path, { code: true })]), pre(stripFence(diff), 'diff')]
    if (fileContent)
      return [
        section([text('Edited '), text(path, { code: true })]),
        pre(fileContent, languageFromPath(path))
      ]
    return [section([text('Edited '), text(path, { code: true })])]
  }
  if (tool.name === 'Read') {
    return [
      section([
        text('Read '),
        text(stringInput(tool.input, 'file_path', stringInput(tool.input, 'path', 'file')), {
          code: true
        })
      ])
    ]
  }
  return [pre(JSON.stringify(tool.input ?? {}, null, 2), 'json')]
}

function outputElementsForResult(result: any): StreamRichTextElement[] {
  let raw = result.content ?? ''
  if (Array.isArray(raw))
    raw = raw
      .map((part: any) => (typeof part === 'string' ? part : (part?.text ?? JSON.stringify(part))))
      .join('\n')
  raw = String(raw ?? '')
  try {
    const parsed = JSON.parse(raw) as any
    if (typeof parsed.diff === 'string') return [pre(stripFence(parsed.diff), 'diff')]
    if (parsed.output !== undefined)
      raw =
        typeof parsed.output === 'string' && parsed.output
          ? parsed.output
          : `exitCode ${parsed.exitCode}`
  } catch {}
  if (raw.includes('\n')) return [pre(clip(raw), languageFromContent(raw))]
  return [section([text(oneLine(raw || (result.is_error ? 'Tool failed' : 'Done')))])]
}

function stripFence(value: string): string {
  return value
    .trim()
    .replace(/^```[a-zA-Z0-9_-]*\n?/, '')
    .replace(/\n?```$/, '')
}

function stringInput(input: any, key: string, fallback = ''): string {
  const value = input?.[key]
  return typeof value === 'string' ? value : fallback
}

function languageFromPath(path: string): string {
  const name = path.split('/').pop() ?? ''
  const extension = name.includes('.') ? name.split('.').pop() : ''
  return extension?.toLowerCase() || 'text'
}

function languageFromContent(value: string): string {
  const trimmed = value.trim()
  if (
    /^(export\s+)?(async\s+)?function\s|^type\s+\w+\s*=|^interface\s+\w+|^const\s+\w+\s*[:=]/m.test(
      trimmed
    )
  )
    return 'ts'
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return 'json'
  return 'text'
}

function clip(value: string, max = 2200): string {
  return value.length > max ? `${value.slice(0, max)}\n/* truncated */` : value
}

function oneLine(value: string, max = 900): string {
  const normalized = value.replace(/\s+/g, ' ').trim()
  return normalized.length > max ? `${normalized.slice(0, max - 1)}…` : normalized
}
