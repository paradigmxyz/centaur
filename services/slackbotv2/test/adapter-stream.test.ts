import { expect, it } from 'bun:test'
import { createSlackAdapter } from '@chat-adapter/slack'
import { ConsoleLogger } from 'chat'

type StreamCall = { method: string; body: Record<string, string>; text: string }

// Exercise the real adapter and Web API stream buffer against a stateful HTTP
// endpoint. Rejected calls do not change visible content, just as an API error
// must not be counted as confirmed delivery by the renderer.
function streamFixture(reject?: (call: StreamCall) => string | undefined) {
  const calls: StreamCall[] = []
  const messages = new Map<string, { text: string; stopped: boolean }>()
  const server = Bun.serve({ port: 0, async fetch(request) {
    const method = new URL(request.url).pathname.split('/').at(-1)!
    const body = Object.fromEntries(new URLSearchParams(await request.text()))
    const chunks = JSON.parse(body.chunks ?? '[]') as Array<{ type: string; text?: string }>
    const text = (body.markdown_text ?? '') + chunks
      .filter(chunk => chunk.type === 'markdown_text').map(chunk => chunk.text ?? '').join('')
    const call = { method, body, text }
    calls.push(call)
    const error = reject?.(call)
    if (error) return Response.json({ ok: false, error })
    if (method === 'chat.startStream') {
      const ts = `100.${messages.size + 1}`
      messages.set(ts, { text, stopped: false })
      return Response.json({ ok: true, ts })
    }
    if (method === 'chat.appendStream' || method === 'chat.stopStream') {
      const message = messages.get(body.ts!)
      if (!message || message.stopped) return Response.json({ ok: false, error: 'message_not_in_streaming_state' })
      message.text += text
      if (method === 'chat.stopStream') message.stopped = true
      return Response.json({ ok: true, ts: body.ts })
    }
    return Response.json({ ok: false, error: 'unknown_method' })
  } })
  const adapter = createSlackAdapter({
    botToken: 'xoxb-fixture', signingSecret: 'fixture', botUserId: 'UBOT', apiUrl: `${server.url}api/`,
    logger: new ConsoleLogger('silent'),
  })
  return { adapter, calls, messages, close: () => server.stop(true) }
}

const streamOptions = { recipientUserId: 'UUSER', recipientTeamId: 'TTEAM' }

it.each(['msg_too_long', 'message_not_in_streaming_state', 'invalid_arguments'])(
  'finalizes confirmed text without retrying a rejected answer after %s', async (errorCode) => {
  let rejected = false
  const fixture = streamFixture(call => {
    if (!rejected && call.text.includes('REJECTED_ANSWER')) {
      rejected = true
      return errorCode
    }
  })
  try {
    await expect(fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield 'Confirmed progress.\n\n'
      yield 'REJECTED_ANSWER'
    })(), streamOptions)).rejects.toMatchObject({ slackAnswerLost: true, slackStreamMessageId: '100.1' })
    expect(rejected).toBe(true)
    expect([...fixture.messages.values()]).toEqual([{ text: 'Confirmed progress.\n\n', stopped: true }])
    // A second transmission would succeed at this endpoint. That would make
    // cleanup publish the rejected answer before durable fallback posts it.
    expect(fixture.calls.filter(call => call.text.includes('REJECTED_ANSWER'))).toHaveLength(1)
  } finally {
    await fixture.close()
  }
})

it.each(['Visible partial answer.\n\n', `${'Partial paragraph.\n\n'.repeat(2_000)}`])(
  'seals partial output and requests durable recovery when the event source fails (case %#)', async (partial) => {
  const fixture = streamFixture()
  const sourceError = new Error('event source disconnected')
  try {
    await expect(fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield partial
      throw sourceError
    })(), streamOptions)).rejects.toBe(sourceError)
    expect(sourceError).toMatchObject({ slackAnswerLost: true })
    expect([...fixture.messages.values()].map(message => message.text).join('')).toBe(partial)
    expect([...fixture.messages.values()].every(message => message.stopped)).toBe(true)
  } finally {
    await fixture.close()
  }
})

it('preserves already visible text when structured progress is rejected mid-stream', async () => {
  const fixture = streamFixture(call => call.body.chunks?.includes('task_update') ? 'invalid_arguments' : undefined)
  try {
    await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield 'Visible introduction.\n\n'
      yield { type: 'task_update' as const, id: 'task', title: 'Working', status: 'in_progress' as const }
      yield 'Final answer.'
    })(), streamOptions)
    const messages = [...fixture.messages.values()]
    expect(messages.map(message => message.text).join('')).toBe('Visible introduction.\n\nFinal answer.')
    expect(messages.every(message => message.stopped)).toBe(true)
    expect(fixture.calls.filter(call => call.body.chunks?.includes('task_update'))).toHaveLength(1)
  } finally {
    await fixture.close()
  }
})

it('delivers a segmented Unicode answer exactly once and closes every physical message', async () => {
  const fixture = streamFixture()
  const answer = `BEGIN\n\n${'🙂 café 日本語 '.repeat(3_000)}\n\nEND`
  try {
    const result = await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      for (const paragraph of answer.split(/(?<=\n\n)/)) yield paragraph
    })(), streamOptions)
    const messages = [...fixture.messages.values()]
    expect(messages.length).toBeGreaterThan(2)
    expect(messages.map(message => message.text).join('')).toBe(answer)
    expect(messages.every(message => message.stopped && message.text.length <= 11_500)).toBe(true)
    expect(result?.id).toBe([...fixture.messages.keys()].at(-1)!)
  } finally {
    await fixture.close()
  }
})

it('balances fences and preserves Unicode content across split code blocks', async () => {
  const fixture = streamFixture()
  const lines = Array.from({ length: 2_000 }, (_, index) => `line ${index}: 🙂 日本語`)
  try {
    await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield '```text\n'
      for (const line of lines) yield `${line}\n`
      yield '```'
    })(), streamOptions)
    const messages = [...fixture.messages.values()]
    expect(messages.length).toBeGreaterThan(2)
    const delivered: string[] = []
    for (const message of messages) {
      expect(message.stopped).toBe(true)
      expect(message.text.startsWith('```text\n')).toBe(true)
      expect(message.text.trimEnd().endsWith('\n```')).toBe(true)
      delivered.push(...message.text.trimEnd().split('\n').slice(1, -1))
    }
    // Closing a fence mid-line inserts a newline at the physical message
    // boundary. This was also true of the 4.31 patch; code-token contents must
    // survive even though a split message is not a byte-exact code download.
    expect(delivered.join('')).toBe(lines.join(''))
  } finally {
    await fixture.close()
  }
})

it('falls back to markdown posts when the first native stream call is unavailable', async () => {
  const calls: Array<{ method: string; body: Record<string, string> }> = []
  const server = Bun.serve({ port: 0, async fetch(request) {
    const method = new URL(request.url).pathname.split('/').at(-1)!
    const body = Object.fromEntries(new URLSearchParams(await request.text()))
    calls.push({ method, body })
    if (method === 'chat.startStream') return Response.json({ ok: false, error: 'unknown_method' })
    return Response.json({ ok: true, ts: '100.1' })
  } })
  const adapter = createSlackAdapter({
    botToken: 'xoxb-fixture', signingSecret: 'fixture', botUserId: 'UBOT', apiUrl: `${server.url}api/`,
    logger: new ConsoleLogger('silent'),
  })
  try {
    const result = await adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield '**First paragraph**\n\n'
      yield 'Final paragraph.'
    })(), { recipientUserId: 'UUSER', recipientTeamId: 'TTEAM', updateIntervalMs: 0 })
    expect(result?.id).toBe('100.1')
    expect(calls.filter(call => call.method === 'chat.startStream')).toHaveLength(1)
    expect(calls.filter(call => call.method === 'chat.postMessage')).toHaveLength(1)
    const final = calls.filter(call => call.method === 'chat.update' || call.method === 'chat.postMessage').at(-1)!
    expect(final.body.markdown_text).toContain('**First paragraph**')
    expect(final.body.markdown_text).toContain('Final paragraph.')
  } finally {
    await server.stop(true)
  }
})

it('continues native text after Slack rejects structured progress', async () => {
  let rejected = 0
  const delivered: string[] = []
  const server = Bun.serve({ port: 0, async fetch(request) {
    const body = Object.fromEntries(new URLSearchParams(await request.text()))
    const chunks = JSON.parse(body.chunks ?? '[]') as Array<{ type: string; text?: string }>
    if (chunks.some(chunk => chunk.type === 'task_update')) {
      rejected++
      return Response.json({ ok: false, error: 'invalid_arguments' })
    }
    if (body.markdown_text) delivered.push(body.markdown_text)
    for (const chunk of chunks) if (chunk.type === 'markdown_text' && chunk.text) delivered.push(chunk.text)
    return Response.json({ ok: true, ts: '100.2' })
  } })
  const adapter = createSlackAdapter({
    botToken: 'xoxb-fixture', signingSecret: 'fixture', botUserId: 'UBOT', apiUrl: `${server.url}api/`,
    logger: new ConsoleLogger('silent'),
  })
  try {
    const result = await adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield { type: 'task_update' as const, id: 'task', title: 'Working', status: 'in_progress' as const }
      yield 'Final answer after unsupported progress.'
    })(), { recipientUserId: 'UUSER', recipientTeamId: 'TTEAM' })
    expect(result?.id).toBe('100.2')
    expect(rejected).toBe(1)
    expect(delivered.join('')).toBe('Final answer after unsupported progress.')
  } finally {
    await server.stop(true)
  }
})
