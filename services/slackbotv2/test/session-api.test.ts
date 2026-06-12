import { describe, expect, test } from 'bun:test'
import { forwardToSessionApi } from '../src/session-api'
import type {
  ForwardSessionInput,
  SlackbotV2ApiMessage,
  SlackbotV2Options
} from '../src/types'

type RecordedRequest = {
  body: unknown
  url: string
}

function apiMessage(text: string): SlackbotV2ApiMessage {
  return {
    attachments: [],
    author: {
      fullName: 'Test User',
      isBot: false,
      isMe: false,
      userId: 'U1',
      userName: 'test'
    },
    id: '1700000000.000100',
    isMention: true,
    raw: {},
    teamId: 'T1',
    text,
    threadId: 'slack:C1:1700000000.000100',
    timestamp: '2026-06-10T00:00:00.000Z'
  }
}

function forwardInput(
  message: SlackbotV2ApiMessage,
  overrides: Partial<ForwardSessionInput> = {}
): ForwardSessionInput {
  return {
    afterEventId: 0,
    executeMessage: message,
    messages: [message],
    onEventId: () => undefined,
    openStream: false,
    threadId: message.threadId,
    ...overrides
  }
}

function fakeApi(responses: { createSession?: Array<{ body?: unknown; status: number }> } = {}) {
  const requests: RecordedRequest[] = []
  const createResponses = [...(responses.createSession ?? [])]
  const fetchFn = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    const body = init?.body ? JSON.parse(String(init.body)) : undefined
    requests.push({ body, url })
    if (url.endsWith('/execute')) {
      return Response.json({
        execution_id: 'exec-1',
        ok: true,
        status: 'running',
        thread_key: 'slack:C1:1700000000.000100'
      })
    }
    if (!url.endsWith('/messages') && createResponses.length > 0) {
      const next = createResponses.shift()!
      return Response.json(next.body ?? { ok: next.status < 400 }, { status: next.status })
    }
    return Response.json({ ok: true })
  }
  return { fetchFn, requests }
}

function options(fetchFn: SlackbotV2Options['fetch']): SlackbotV2Options {
  return {
    apiUrl: 'http://api.test',
    botToken: 'xoxb-test',
    fetch: fetchFn,
    signingSecret: 'secret'
  }
}

describe('forwardToSessionApi overrides', () => {
  test('creates session with default codex harness', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const create = requests.find(request => request.url.endsWith('.000100'))
    expect((create?.body as { harness_type?: string }).harness_type).toBe('codex')
  })

  test('creates session with parsed harness override', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('review this'), { harnessType: 'claudecode' })
    )
    const create = requests.find(request => request.url.endsWith('.000100'))
    expect((create?.body as { harness_type?: string }).harness_type).toBe('claudecode')
  })

  test('includes model override on the execute input line', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('review this'), {
        harnessType: 'claudecode',
        model: 'claude-sonnet-4-6'
      })
    )
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const inputLines = (execute?.body as { input_lines: string[] }).input_lines
    expect(inputLines).toHaveLength(1)
    const line = JSON.parse(inputLines[0]!)
    expect(line.model).toBe('claude-sonnet-4-6')
    expect(line.message.content[0].text).toContain('# Requester Context')
    expect(line.message.content.at(-1)).toEqual({ type: 'text', text: 'review this' })
  })

  test('omits model field when no override is set', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    expect('model' in line).toBe(false)
  })

  test('retries session creation with existing harness on 409 conflict', async () => {
    const { fetchFn, requests } = fakeApi({
      createSession: [
        {
          body: {
            code: 'harness_conflict',
            error:
              'session slack:C1:1700000000.000100 already exists with harness_type codex, requested claudecode',
            existing_harness: 'codex',
            ok: false,
            requested_harness: 'claudecode'
          },
          status: 409
        },
        { status: 200 }
      ]
    })
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('review this'), { harnessType: 'claudecode' })
    )
    const creates = requests.filter(request => request.url.endsWith('.000100'))
    expect(creates.map(request => (request.body as { harness_type: string }).harness_type)).toEqual(
      ['claudecode', 'codex']
    )
    expect(requests.some(request => request.url.endsWith('/execute'))).toBe(true)
  })

  test('recovers existing harness from the error message when fields are absent', async () => {
    const { fetchFn, requests } = fakeApi({
      createSession: [
        {
          body: {
            error:
              'session slack:C1:1700000000.000100 already exists with harness_type amp, requested codex',
            ok: false
          },
          status: 409
        },
        { status: 200 }
      ]
    })
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const creates = requests.filter(request => request.url.endsWith('.000100'))
    expect(creates.map(request => (request.body as { harness_type: string }).harness_type)).toEqual(
      ['codex', 'amp']
    )
  })

  test('surfaces non-conflict create failures', async () => {
    const { fetchFn } = fakeApi({
      createSession: [{ body: { error: 'boom', ok: false }, status: 500 }]
    })
    await expect(
      forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    ).rejects.toThrow('create session failed: 500')
  })
})
