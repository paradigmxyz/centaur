import { expect, it } from 'bun:test'
import { createSlackAdapter } from '@chat-adapter/slack'
import { ConsoleLogger } from 'chat'

type StreamCall = { method: string; body: Record<string, string>; text: string }

// Exercise the real adapter and Web API stream buffer against a stateful HTTP
// endpoint. Rejected calls do not change visible content, just as an API error
// must not be counted as confirmed delivery by the renderer.
function streamFixture(reject?: (call: StreamCall) => string | undefined, streamSegmentMaxAgeMs?: number) {
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
    streamSegmentMaxAgeMs,
  })
  return { adapter, calls, messages, close: () => server.stop(true) }
}

const streamOptions = { recipientUserId: 'UUSER', recipientTeamId: 'TTEAM' }
const footer = { type: 'context' as const, elements: [{ type: 'mrkdwn' as const, text: 'Response context' }] }

it.each([false, true])('requests durable recovery when cards are rejected in an empty segment (rotation: %s)', async (rotate) => {
  const fixture = streamFixture(call => call.body.chunks?.includes('rejected-task') ? 'invalid_arguments' : undefined, 1)
  try {
    await expect(fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      if (rotate) {
        yield { type: 'task_update' as const, id: 'accepted-task', title: 'Done', status: 'complete' as const }
        await Bun.sleep(10)
      }
      yield { type: 'task_update' as const, id: 'rejected-task', title: 'Working', status: 'in_progress' as const }
    })(), { ...streamOptions, stopBlocks: [footer] })).rejects.toMatchObject({ slackAnswerLost: true })
    expect([...fixture.messages.values()].every(message => message.stopped)).toBe(true)
    expect(fixture.calls.some(call => call.body.chunks?.includes('rejected-task'))).toBe(true)
  } finally {
    await fixture.close()
  }
})

it.each(['Buffered final answer.\n\n', 'Confirmed answer.\n\n'.repeat(30)])(
  'does not request a duplicate answer when cleanup recovers a transient stop failure (case %#)', async (answer) => {
  let stopFailures = 0
  const fixture = streamFixture(call => {
    if (call.method === 'chat.stopStream' && stopFailures++ === 0) return 'internal_error'
  })
  try {
    await expect(fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield answer
    })(), streamOptions)).rejects.toMatchObject({ slackAnswerLost: false })
    expect(stopFailures).toBe(2)
    expect([...fixture.messages.values()]).toEqual([{ text: answer, stopped: true }])
  } finally {
    await fixture.close()
  }
})

it('coalesces short markdown deltas while delivering the complete answer exactly once', async () => {
  const fixture = streamFixture()
  const paragraphs = Array.from({ length: 100 }, (_, i) => `Line ${i}.\n\n`)
  try {
    await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield* paragraphs
    })(), streamOptions)
    expect([...fixture.messages.values()]).toEqual([{ text: paragraphs.join(''), stopped: true }])
    expect(fixture.calls.length).toBeLessThan(10)
  } finally {
    await fixture.close()
  }
})

it('requests recovery when a rejected progress update strands buffered answer text', async () => {
  const fixture = streamFixture(call => call.body.chunks?.includes('rejected-task') ? 'invalid_arguments' : undefined)
  try {
    await expect(fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield { type: 'task_update' as const, id: 'accepted-task', title: 'Done', status: 'complete' as const }
      yield 'Buffered answer.\n\n'
      yield { type: 'task_update' as const, id: 'rejected-task', title: 'Working', status: 'in_progress' as const }
    })(), streamOptions)).rejects.toMatchObject({ slackAnswerLost: true })
    expect([...fixture.messages.values()]).toEqual([{ text: '', stopped: true }])
    expect(fixture.calls.filter(call => call.text.includes('Buffered answer.'))).toHaveLength(1)
  } finally {
    await fixture.close()
  }
})

it.each([false, true])('finishes an age rotation that flushes the entire buffered tail (footer: %s)', async (withFooter) => {
  const fixture = streamFixture(undefined, 100)
  const first = 'First paragraph.\n\n'.repeat(20)
  const tail = 'Buffered tail.\n\n'
  const last = 'Last paragraph.\n\n'
  try {
    const result = await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield first
      yield tail
      await Bun.sleep(125)
      yield last
    })(), { ...streamOptions, ...(withFooter ? { stopBlocks: [footer] } : {}) })
    const messages = [...fixture.messages.values()]
    expect(messages.map(message => message.text).join('')).toBe(first + tail + last)
    expect(messages.every(message => message.stopped)).toBe(true)
    expect(messages).toHaveLength(withFooter ? 2 : 1)
    expect(result?.id).toBe(withFooter ? '100.2' : '100.1')
    if (withFooter) {
      expect(JSON.parse(fixture.calls.at(-1)!.body.blocks!)).toEqual([footer])
    }
  } finally {
    await fixture.close()
  }
})

it('does not replay a confirmed message prefix when a buffered tail expires at stop', async () => {
  let expired = false
  const fixture = streamFixture(call => {
    if (!expired && call.method === 'chat.stopStream') {
      expired = true
      return 'message_not_in_streaming_state'
    }
  })
  const prefix = 'Confirmed paragraph.\n\n'.repeat(20)
  const tail = 'Buffered tail.'
  try {
    await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield prefix
      yield tail
    })(), streamOptions)
    expect([...fixture.messages.values()].map(message => message.text).join('')).toBe(prefix + tail)
    expect([...fixture.messages.values()].at(-1)?.stopped).toBe(true)
  } finally {
    await fixture.close()
  }
})

it.each(['msg_too_long', 'message_not_in_streaming_state', 'invalid_arguments'])(
  'finalizes confirmed text without retrying a rejected answer after %s', async (errorCode) => {
  let rejected = false
  const confirmed = 'Confirmed progress.\n\n'.repeat(20)
  const fixture = streamFixture(call => {
    if (!rejected && call.text.includes('REJECTED_ANSWER')) {
      rejected = true
      return errorCode
    }
  })
  try {
    await expect(fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield confirmed
      yield 'REJECTED_ANSWER'.repeat(30)
    })(), streamOptions)).rejects.toMatchObject({ slackAnswerLost: true, slackStreamMessageId: '100.1' })
    expect(rejected).toBe(true)
    expect([...fixture.messages.values()]).toEqual([{ text: confirmed, stopped: true }])
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
  const introduction = 'Visible introduction.\n\n'.repeat(20)
  try {
    await fixture.adapter.stream('slack:CCHANNEL:100', (async function* () {
      yield introduction
      yield { type: 'task_update' as const, id: 'task', title: 'Working', status: 'in_progress' as const }
      yield 'Final answer.'
    })(), streamOptions)
    const messages = [...fixture.messages.values()]
    expect(messages.map(message => message.text).join('')).toBe(`${introduction}Final answer.`)
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
