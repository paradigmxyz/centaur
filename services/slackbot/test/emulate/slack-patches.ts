import type { Emulator } from 'emulate'

type PatchedSlack = {
  url: string
  close(): Promise<void>
}

type StreamState = {
  channel: string
  ts: string
  text: string
}

const STREAMS = new Map<string, StreamState>()

export async function createPatchedSlackApi(emulator: Emulator): Promise<PatchedSlack> {
  const port = await availablePort(4013)
  const server = Bun.serve({
    port,
    async fetch(request: Request) {
      const url = new URL(request.url)
      const pathname = normalizeSlackPath(url.pathname)
      if (pathname === '/api/assistant.threads.setStatus') {
        // Emulate 0.5.0 does not implement Slack assistant.threads.setStatus.
        // Remove this patch when https://emulate.dev/docs/slack lists the endpoint.
        return slackOk()
      }
      if (pathname === '/api/assistant.threads.setTitle') {
        // Emulate 0.5.0 does not implement Slack assistant.threads.setTitle.
        // Remove this patch when https://emulate.dev/docs/slack lists the endpoint.
        return slackOk()
      }
      if (pathname === '/api/chat.startStream') {
        // Emulate 0.5.0 does not implement Slack chat.startStream.
        // This maps streams onto chat.postMessage so E2E tests can still inspect state.
        return startStream(emulator.url, request)
      }
      if (pathname === '/api/chat.appendStream') {
        // Emulate 0.5.0 does not implement Slack chat.appendStream.
        // This accumulates chunks into the message created by chat.startStream.
        return appendStream(emulator.url, request)
      }
      if (pathname === '/api/chat.stopStream') {
        // Emulate 0.5.0 does not implement Slack chat.stopStream.
        // This finalizes the accumulated stream text through chat.update.
        return stopStream(emulator.url, request)
      }
      return fetch(new URL(`${pathname}${url.search}`, emulator.url), {
        method: request.method,
        headers: request.headers,
        body: request.body
      })
    }
  })

  return {
    url: `http://localhost:${server.port}`,
    close: async () => {
      await server.stop()
    }
  }
}

function normalizeSlackPath(pathname: string): string {
  return pathname.startsWith('/api/') ? pathname : `/api${pathname}`
}

async function availablePort(preferred: number): Promise<number> {
  for (let port = preferred; port < preferred + 100; port++) {
    if (!(await isPortOpen(port))) return port
  }
  throw new Error(`No available port near ${preferred}`)
}

async function isPortOpen(port: number): Promise<boolean> {
  const { connect } = await import('node:net')
  return new Promise(resolve => {
    const socket = connect(port, '127.0.0.1')
    socket.once('connect', () => {
      socket.destroy()
      resolve(true)
    })
    socket.once('error', () => resolve(false))
    socket.setTimeout(250, () => {
      socket.destroy()
      resolve(false)
    })
  })
}

async function startStream(emulatorUrl: string, request: Request): Promise<Response> {
  const body = await slackBody(request)
  const channel = stringField(body.channel)
  const threadTs = stringField(body.thread_ts)
  const text = chunksText(body.chunks) || ' '
  const posted = await slackFetch(emulatorUrl, request, '/api/chat.postMessage', {
    channel,
    thread_ts: threadTs || undefined,
    text
  })
  if (!posted.ok) return Response.json(posted)
  const ts = stringField(posted.ts)
  STREAMS.set(streamKey(channel, ts), { channel, ts, text })
  return Response.json({ ok: true, channel, ts })
}

async function appendStream(emulatorUrl: string, request: Request): Promise<Response> {
  const body = await slackBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  const key = streamKey(channel, ts)
  const existing = STREAMS.get(key) ?? { channel, ts, text: '' }
  existing.text += chunksText(body.chunks)
  STREAMS.set(key, existing)
  await slackFetch(emulatorUrl, request, '/api/chat.update', {
    channel,
    ts,
    text: existing.text || ' '
  })
  return Response.json({ ok: true, channel, ts })
}

async function stopStream(emulatorUrl: string, request: Request): Promise<Response> {
  const body = await slackBody(request)
  const channel = stringField(body.channel)
  const ts = stringField(body.ts)
  const key = streamKey(channel, ts)
  const existing = STREAMS.get(key) ?? { channel, ts, text: '' }
  const finalText = [existing.text, blocksText(body.blocks), chunksText(body.chunks)]
    .filter(text => text.trim())
    .join('\n')
  await slackFetch(emulatorUrl, request, '/api/chat.update', {
    channel,
    ts,
    text: finalText || existing.text || ' '
  })
  STREAMS.delete(key)
  return Response.json({ ok: true, channel, ts })
}

async function slackBody(request: Request): Promise<Record<string, unknown>> {
  const contentType = request.headers.get('content-type') ?? ''
  const text = await request.text()
  if (contentType.includes('application/json')) {
    return JSON.parse(text || '{}') as Record<string, unknown>
  }
  const params = new URLSearchParams(text)
  return Object.fromEntries(
    Array.from(params.entries()).map(([key, value]) => [key, parseMaybeJson(value)])
  )
}

async function slackFetch(
  emulatorUrl: string,
  original: Request,
  path: string,
  body: Record<string, unknown>
): Promise<Record<string, unknown>> {
  const response = await fetch(new URL(path, emulatorUrl), {
    method: 'POST',
    headers: {
      authorization: original.headers.get('authorization') ?? '',
      'content-type': 'application/json'
    },
    body: JSON.stringify(body)
  })
  return (await response.json()) as Record<string, unknown>
}

function slackOk(): Response {
  return Response.json({ ok: true })
}

function streamKey(channel: string, ts: string): string {
  return `${channel}:${ts}`
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function parseMaybeJson(value: string): unknown {
  const trimmed = value.trim()
  if (!trimmed || !['[', '{'].includes(trimmed[0] ?? '')) return value
  try {
    return JSON.parse(trimmed)
  } catch {
    return value
  }
}

function chunksText(value: unknown): string {
  if (!Array.isArray(value)) return ''
  return value
    .map(chunk => {
      if (!chunk || typeof chunk !== 'object') return ''
      const item = chunk as Record<string, unknown>
      if (typeof item.text === 'string') return item.text
      if (typeof item.title === 'string') return item.title
      if (typeof item.output === 'string') return item.output
      if (typeof item.details === 'string') return item.details
      return blocksText(item.blocks)
    })
    .filter(Boolean)
    .join('\n')
}

function blocksText(value: unknown): string {
  if (!Array.isArray(value)) return ''
  return value.map(blockText).filter(Boolean).join('\n')
}

function blockText(block: unknown): string {
  if (!block || typeof block !== 'object') return ''
  const item = block as Record<string, unknown>
  if (typeof item.text === 'string') return item.text
  const text = item.text
  if (
    text &&
    typeof text === 'object' &&
    typeof (text as Record<string, unknown>).text === 'string'
  ) {
    return String((text as Record<string, unknown>).text)
  }
  if (Array.isArray(item.elements)) return item.elements.map(blockText).filter(Boolean).join('\n')
  return ''
}
