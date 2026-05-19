// @ts-nocheck

import { preformatted as pre, section, text } from '../src/slack/streaming'

const slackbotApiUrl = (process.env.SLACKBOT_API_URL ?? 'http://localhost:6969').replace(/\/$/, '')
const slackbotApiKey = process.env.SLACKBOT_API_KEY
const channel = process.env.SLACK_CHANNEL_ID
const recipientTeamId = process.env.SLACK_RECIPIENT_TEAM_ID
const recipientUserId = process.env.SLACK_RECIPIENT_USER_ID
const prompt = Bun.argv.slice(2).join(' ').trim()

if (!slackbotApiKey) throw new Error('SLACKBOT_API_KEY is required')
if (!channel) throw new Error('SLACK_CHANNEL_ID is required')
if (!recipientTeamId)
  throw new Error('SLACK_RECIPIENT_TEAM_ID is required for Slack native streams')
if (!recipientUserId)
  throw new Error('SLACK_RECIPIENT_USER_ID is required for Slack native streams')
if (!prompt) throw new Error('Usage: bun examples/amp-full-stream.ts "prompt"')

const tasks = []
const timeline = []
const messageParts = []
const taskByUseId = new Map()

let sessionId = 'unknown'
let agentSessionId
let stepCounter = 0
let stderr = ''

const parent = await slackbot('POST', '/api/slack/messages', {
  channel,
  text: 'Amp live stream demo',
  blocks: [
    {
      type: 'markdown',
      text: '**Amp live stream demo**\n\nOne native Slack stream with plan steps, assistant text, and rich-text code blocks.'
    }
  ]
})

await slackbot('POST', '/api/slack/assistant/title', {
  channel_id: channel,
  thread_ts: parent.ts,
  title: 'amp live stream demo'
})
await setThinking()

const opened = await slackbot('POST', '/api/slack/agent-sessions', {
  channel,
  parent_ts: parent.ts,
  recipient_team_id: recipientTeamId,
  recipient_user_id: recipientUserId,
  title: 'Amp execution steps',
  header: 'base · amp'
})
agentSessionId = opened.session_id

const amp = Bun.spawn(['amp', '--execute', '--stream-json-thinking', '--dangerously-allow-all'], {
  cwd: process.cwd(),
  stdin: 'pipe',
  stdout: 'pipe',
  stderr: 'pipe'
})

void amp.stdin.write(`${prompt}\n`)
void amp.stdin.end()

const stderrPromise = readText(amp.stderr).then(text => {
  stderr = text
  if (text) console.error('amp-stderr', text)
})

for await (const line of linesFromStream(amp.stdout)) {
  if (!line.trim()) continue
  try {
    await handleAmpEvent(JSON.parse(line))
  } catch (error) {
    console.error('event-failed', error instanceof Error ? error.message : error, line)
  }
}

const exitCode = await amp.exited
await stderrPromise

if (exitCode !== 0) {
  tasks.push({
    kind: 'task',
    id: `task-${++stepCounter}`,
    title: 'Amp failed',
    status: 'error',
    details: [section([text(oneLine(stderr))])],
    output: []
  })
  timeline.push(tasks.at(-1))
  messageParts.push(`Amp exited with code ${exitCode}`)
  timeline.push({
    kind: 'message',
    id: `message-${timeline.length + 1}`,
    text: messageParts.at(-1)
  })
  await publishTask(tasks.at(-1))
  await stopAssistantStream()
  await clearStatus()
}

console.log(
  JSON.stringify(
    {
      parent_ts: parent.ts,
      agent_session_id: agentSessionId,
      amp_thread_id: sessionId,
      exit_code: exitCode
    },
    null,
    2
  )
)

async function handleAmpEvent(event) {
  console.log(
    'amp',
    JSON.stringify({
      type: event.type,
      subtype: event.subtype,
      session_id: event.session_id,
      stop_reason: event.message?.stop_reason
    })
  )

  if (event.session_id) sessionId = event.session_id
  if (hasThinking(event)) await setThinking()

  for (const tool of toolUses(event)) {
    const task = {
      kind: 'task',
      id: `task-${++stepCounter}`,
      title: titleFor(tool),
      status: 'in_progress',
      details: detailElementsForTool(tool),
      output: []
    }
    tasks.push(task)
    timeline.push(task)
    taskByUseId.set(tool.id, task)
    await publishTask(task)
  }

  for (const result of toolResults(event)) {
    const task = taskByUseId.get(result.tool_use_id) ?? {
      kind: 'task',
      id: `task-${++stepCounter}`,
      title: 'Tool result',
      status: 'in_progress',
      details: [],
      output: []
    }

    if (!tasks.includes(task)) {
      tasks.push(task)
      timeline.push(task)
    }
    task.status = result.is_error ? 'error' : 'complete'
    task.output = outputElementsForResult(result)
    await publishTask(task)
  }

  const assistantMessage = assistantText(event).trim()
  if (assistantMessage) {
    messageParts.push(assistantMessage)
    await streamAssistant(assistantMessage)
  }

  if (event.type === 'result') {
    if (typeof event.result === 'string' && !messageParts.length) {
      const resultText = event.result.trim()
      if (resultText) {
        messageParts.push(resultText)
        await streamAssistant(resultText)
      }
    }
    await stopAssistantStream()
    await clearStatus()
  }
}

async function publishTask(task) {
  await slackbot('POST', `/api/slack/agent-sessions/${agentSessionId}/step`, {
    id: task.id,
    title: task.title,
    status: task.status === 'in_progress' ? 'in_progress' : task.status,
    details: elementsToMarkdown(task.details),
    output: elementsToMarkdown(task.output)
  })
}

async function streamAssistant(markdown) {
  if (!markdown.trim()) return
  await slackbot('POST', `/api/slack/agent-sessions/${agentSessionId}/text`, { markdown })
}

async function stopAssistantStream() {
  await slackbot('POST', `/api/slack/agent-sessions/${agentSessionId}/done`, {
    thread_id: sessionId
  })
}

async function slackbot(method, path, body) {
  const init = {
    method,
    headers: {
      authorization: `Bearer ${slackbotApiKey}`,
      'content-type': 'application/json; charset=utf-8'
    }
  }
  if (method !== 'GET') init.body = JSON.stringify(body)
  const response = await fetch(`${slackbotApiUrl}${path}`, init)
  const responseText = await response.text()
  let result
  try {
    result = responseText ? JSON.parse(responseText) : { ok: response.ok }
  } catch {
    throw new Error(
      `Slackbot ${path} returned non-JSON ${response.status}: ${responseText.slice(0, 500)}`
    )
  }
  console.log(
    'slackbot',
    path,
    JSON.stringify({ ok: result.ok, error: result.error, channel: result.channel, ts: result.ts })
  )
  if (!response.ok || !result.ok) {
    throw new Error(`Slackbot ${path} failed: ${result.error ?? response.status}`)
  }
  return result
}

async function setThinking() {
  await slackbot('POST', '/api/slack/assistant/status', {
    channel_id: channel,
    thread_ts: parent.ts,
    status: 'thinking',
    loading_messages: ['Thinking']
  })
}

async function clearStatus() {
  await slackbot('POST', '/api/slack/assistant/status', {
    channel_id: channel,
    thread_ts: parent.ts,
    status: ''
  })
}

function content(event) {
  return event.message?.content ?? []
}

function assistantText(event) {
  if (event.type !== 'assistant') return ''
  return content(event)
    .map(part => (part.type === 'text' ? (part.text ?? '') : ''))
    .filter(Boolean)
    .join('\n')
}

function hasThinking(event) {
  return event.type === 'assistant' && content(event).some(part => part.type === 'thinking')
}

function toolUses(event) {
  if (event.type !== 'assistant') return []
  return content(event).filter(part => part.type === 'tool_use')
}

function toolResults(event) {
  if (event.type !== 'user') return []
  return content(event).filter(part => part.type === 'tool_result')
}

function elementsToMarkdown(elements) {
  return elements.map(elementToMarkdown).filter(Boolean).join('\n\n')
}

function elementToMarkdown(element) {
  if (element.type === 'rich_text_preformatted') {
    const body = element.elements?.map(inline => inline.text ?? '').join('') ?? ''
    return `\`\`\`${element.language ?? ''}\n${body}\n\`\`\``
  }
  if (element.type === 'rich_text_section') {
    return (element.elements ?? [])
      .map(inline => {
        const value = inline.text ?? ''
        return inline.style?.code ? `\`${value}\`` : value
      })
      .join('')
  }
  return ''
}

function titleFor(tool) {
  if (tool.name === 'Bash') return 'Run command'
  if (tool.name === 'create_file') return 'Create file'
  if (tool.name === 'edit_file') return 'Edit file'
  return `Use ${tool.name ?? 'tool'}`
}

function detailElementsForTool(tool) {
  if (tool.name === 'Bash') return [pre(`$ ${stringInput(tool.input, 'cmd')}`, 'bash')]
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
    if (newStr) {
      return [
        section([text('Edited '), text(path, { code: true })]),
        pre(newStr, languageFromPath(path))
      ]
    }
    if (diff) {
      return [section([text('Edited '), text(path, { code: true })]), pre(stripFence(diff), 'diff')]
    }
    if (fileContent) {
      return [
        section([text('Edited '), text(path, { code: true })]),
        pre(fileContent, languageFromPath(path))
      ]
    }
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

function outputElementsForResult(result) {
  let raw = result.content ?? ''
  try {
    const parsed = JSON.parse(raw)
    if (typeof parsed.diff === 'string') return [pre(stripFence(parsed.diff), 'diff')]
    if (parsed.output !== undefined) {
      raw =
        typeof parsed.output === 'string' && parsed.output
          ? parsed.output
          : `exitCode ${parsed.exitCode}`
    }
  } catch {}

  if (raw.includes('\n')) return [pre(raw, languageFromContent(raw))]
  return [section([text(oneLine(raw || (result.is_error ? 'Tool failed' : 'Done')))])]
}

function stripFence(value) {
  return value
    .trim()
    .replace(/^```[a-zA-Z0-9_-]*\n?/, '')
    .replace(/\n?```$/, '')
}

function stringInput(input, key, fallback = '') {
  const value = input?.[key]
  return typeof value === 'string' ? value : fallback
}

function languageFromPath(path) {
  const name = path.split('/').pop() ?? ''
  const extension = name.includes('.') ? name.split('.').pop() : ''
  return extension?.toLowerCase() || 'text'
}

function languageFromContent(value) {
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

function oneLine(value, max = 900) {
  const text = value.replace(/\s+/g, ' ').trim()
  return text.length > max ? `${text.slice(0, max - 1)}…` : text
}

async function readText(stream) {
  return await new Response(stream).text()
}

async function* linesFromStream(stream) {
  const reader = stream.getReader()
  const decoder = new TextDecoder()
  let buffer = ''

  while (true) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split('\n')
    buffer = lines.pop() ?? ''
    for (const line of lines) yield line
  }

  buffer += decoder.decode()
  if (buffer) yield buffer
}
