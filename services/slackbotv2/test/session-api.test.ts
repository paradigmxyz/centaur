import { describe, expect, test } from 'bun:test'
import {
  clearConversationNameCacheForTests,
  clearRequesterIdentityCacheForTests,
  DEFAULT_SESSION_IDLE_TIMEOUT_MS,
  forwardToSessionApi,
  harnessRestartPreamble,
  interruptSessionExecution,
  openSessionEventStream,
  serializeAttachment,
  serializeMessage
} from '../src/session-api'
import { renderSlackDisplayText } from '../src/slack-display-text'
import type {
  ForwardSessionInput,
  JsonObject,
  JsonValue,
  SlackbotV2ApiMessage,
  SlackbotV2Options
} from '../src/types'

type RecordedRequest = {
  body: unknown
  url: string
}

function apiMessage(
  text: string,
  overrides: Partial<SlackbotV2ApiMessage> = {}
): SlackbotV2ApiMessage {
  const raw = overrides.raw ?? {}
  const displayText = renderSlackDisplayText({ raw, text })
  return {
    attachments: [],
    author: {
      fullName: 'Test User',
      isBot: false,
      isMe: false,
      userId: 'U1',
      userName: 'test'
    },
    displayText: displayText.text,
    displayTextSource: displayText.source,
    id: '1700000000.000100',
    isMention: true,
    raw,
    rawSlackAttachmentCount: displayText.rawAttachmentCount,
    rawSlackBlockCount: displayText.rawBlockCount,
    teamId: 'T1',
    text,
    threadId: 'slack:C1:1700000000.000100',
    timestamp: '2026-06-10T00:00:00.000Z',
    ...overrides
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
    if (url.endsWith('/interrupt')) {
      return Response.json({
        execution_id: 'exec-1',
        interrupted: true,
        ok: true,
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

function executeBody(requests: RecordedRequest[]): Record<string, unknown> {
  const execute = requests.find(request => request.url.endsWith('/execute'))
  return (execute?.body ?? {}) as Record<string, unknown>
}

function executeLine(requests: RecordedRequest[]): JsonObject {
  const inputLines = (executeBody(requests) as { input_lines: string[] }).input_lines
  return JSON.parse(inputLines[0]!) as JsonObject
}

function appendedTextParts(requests: RecordedRequest[]): string[] {
  const append = requests.find(request => request.url.endsWith('/messages'))
  const messages = ((append?.body as { messages?: Array<{ parts?: JsonValue[] }> }).messages ?? [])
  return messages.flatMap(message =>
    (message.parts ?? []).flatMap(part =>
      isJsonRecord(part) && part.type === 'text' && typeof part.text === 'string'
        ? [part.text]
        : []
    )
  )
}

function lineContent(line: JsonObject): JsonObject[] {
  const message = line.message
  if (!isJsonRecord(message)) return []
  return Array.isArray(message.content) ? message.content.filter(isJsonRecord) : []
}

function textPartIncludes(part: JsonObject, text: string): boolean {
  return typeof part.text === 'string' && part.text.includes(text)
}

function isJsonRecord(value: JsonValue | undefined): value is JsonObject {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

describe('session event streaming', () => {
  test('passes activity summary events through to the renderer source stream', async () => {
    const encoded = new TextEncoder().encode(
      [
        'id: 1',
        'event: session.activity_summary',
        'data: {"summary":"The agent is reading App Server events."}',
        '',
        'id: 2',
        'event: session.execution_completed',
        'data: {"result_text":"done"}',
        '',
      ].join('\n')
    )
    const fetchFn: SlackbotV2Options['fetch'] = async () =>
      new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(encoded)
            controller.close()
          }
        }),
        { headers: { 'content-type': 'text/event-stream' } }
      )
    const seenEventIds: number[] = []

    const stream = await openSessionEventStream(options(fetchFn), {
      afterEventId: 0,
      executionId: 'exec-1',
      onEventId: eventId => seenEventIds.push(eventId),
      threadId: 'slack:C1:1700000000.000100'
    })
    const events = []
    for await (const event of stream) events.push(event)

    expect(events[0]).toEqual({
      data: { summary: 'The agent is reading App Server events.' },
      event: 'session.activity_summary',
      eventId: 1,
      eventKind: 'session.activity_summary'
    })
    expect(events[1]).toMatchObject({
      event: 'session.execution_completed',
      eventId: 2,
      eventKind: 'session.execution_completed'
    })
    expect(seenEventIds).toEqual([1, 2])
  })

  test('uses interrupted wording for cancelled executions without error text', async () => {
    const encoded = new TextEncoder().encode(
      [
        'id: 1',
        'event: session.execution_cancelled',
        'data: {"status":"cancelled","reason":"turn_interrupted"}',
        '',
      ].join('\n')
    )
    const fetchFn: SlackbotV2Options['fetch'] = async () =>
      new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(encoded)
            controller.close()
          }
        }),
        { headers: { 'content-type': 'text/event-stream' } }
      )
    const seenEventIds: number[] = []

    const stream = await openSessionEventStream(options(fetchFn), {
      afterEventId: 0,
      executionId: 'exec-1',
      onEventId: eventId => seenEventIds.push(eventId),
      threadId: 'slack:C1:1700000000.000100'
    })
    const events = []
    for await (const event of stream) events.push(event)

    expect(events).toEqual([
      {
        data: { error: 'Execution interrupted' },
        event: 'session.execution_cancelled',
        eventId: 1,
        eventKind: 'session.execution_cancelled'
      }
    ])
    expect(seenEventIds).toEqual([1])
  })
})

describe('session interruption', () => {
  test('posts interruption reason to the thread interrupt endpoint', async () => {
    const { fetchFn, requests } = fakeApi()

    const response = await interruptSessionExecution(
      options(fetchFn),
      'slack:C1:1700000000.000100',
      'Interrupted from Slack by U1'
    )

    expect(response.interrupted).toBe(true)
    const interrupt = requests.find(request => request.url.endsWith('/interrupt'))
    expect(interrupt?.url).toBe(
      'http://api.test/api/session/slack%3AC1%3A1700000000.000100/interrupt'
    )
    expect(interrupt?.body).toEqual({ reason: 'Interrupted from Slack by U1' })
  })
})

describe('Slack display text fallback', () => {
  test('serializeMessage extracts raw Slack blocks when adapter text is empty', async () => {
    const raw = {
      blocks: [
        {
          text: { text: '*Alert:* <https://example.test/incident|prod down>', type: 'mrkdwn' },
          type: 'section'
        },
        {
          elements: [{ text: 'prd-centaur-na', type: 'mrkdwn' }],
          type: 'context'
        }
      ],
      team_id: 'T1'
    }
    const message = await serializeMessage({
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
      metadata: { dateSent: new Date('2026-06-10T00:00:00.000Z') },
      raw,
      text: '',
      threadId: 'slack:C1:1700000000.000100'
    } as unknown as Parameters<typeof serializeMessage>[0])

    expect(message.displayTextSource).toBe('raw_blocks')
    expect(message.displayText).toBe(
      '*Alert:* prod down (https://example.test/incident)\nprd-centaur-na'
    )
    expect(message.rawSlackBlockCount).toBe(2)
  })

  test('forwards raw Slack block text to session parts and Codex input', async () => {
    const { fetchFn, requests } = fakeApi()
    const message = apiMessage('', {
      raw: {
        blocks: [
          {
            text: { text: '*Alert:* API errors above threshold', type: 'mrkdwn' },
            type: 'section'
          },
          {
            fields: [
              { text: '*service*\ncodex-app-server', type: 'mrkdwn' },
              { text: '*sandbox*\nprd-centaur-na', type: 'mrkdwn' }
            ],
            type: 'section'
          }
        ]
      }
    })

    await forwardToSessionApi(options(fetchFn), forwardInput(message))

    const expected = '*Alert:* API errors above threshold\n*service*\ncodex-app-server\n*sandbox*\nprd-centaur-na'
    expect(appendedTextParts(requests)).toContain(expected)

    const line = executeLine(requests)
    expect(line.trace_metadata).toEqual(
      expect.objectContaining({
        slack_display_text_chars: expected.length,
        slack_raw_block_count: 2,
        slack_text_source: 'raw_blocks'
      })
    )
    expect(lineContent(line).at(-1)).toEqual({ type: 'text', text: expected })
  })

  test('includes API-owned Slack upload destination in visible Codex input', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('make an image')))

    const context = lineContent(executeLine(requests)).find(part =>
      typeof part.text === 'string' && part.text.includes('# Slack Session Context')
    )
    expect(context?.text).toContain('session_context.slack.channel_id: C1')
    expect(context?.text).toContain('session_context.slack.thread_ts: 1700000000.000100')
    expect(context?.text).toContain('thread_key: slack:C1:1700000000.000100')
    expect(context?.text).toContain('slack upload C1 /path/to/file --thread 1700000000.000100')
    expect(context?.text).toContain('Do not recover this destination with Slack search.')
  })

  test('includes team id for team-qualified Slack thread keys', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('make an image', { threadId: 'slack:T1:C1:1700000000.000100' }))
    )

    const context = lineContent(executeLine(requests)).find(part =>
      typeof part.text === 'string' && part.text.includes('# Slack Session Context')
    )
    expect(context?.text).toContain('session_context.slack.team_id: T1')
    expect(context?.text).toContain('session_context.slack.channel_id: C1')
    expect(context?.text).toContain('session_context.slack.thread_ts: 1700000000.000100')
    expect(context?.text).toContain('thread_key: slack:T1:C1:1700000000.000100')
  })

  test('uses raw Slack block text in prior thread context instead of no text', async () => {
    const { fetchFn, requests } = fakeApi()
    const root = apiMessage('', {
      id: '1700000000.000001',
      isMention: false,
      raw: {
        blocks: [
          {
            text: { text: 'Incident: queue stalled in prd-centaur-na', type: 'mrkdwn' },
            type: 'section'
          }
        ]
      }
    })
    const current = apiMessage('investigate', { id: '1700000000.000002' })

    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(current, {
        executeContextMessages: [root, current],
        messages: [root, current]
      })
    )

    const context = lineContent(executeLine(requests)).find(part =>
      typeof part.text === 'string' && part.text.includes('# Slack Thread Context')
    )
    expect(context?.text).toContain('Incident: queue stalled in prd-centaur-na')
    expect(context?.text).not.toContain('[no text]')
  })

  test('falls back to raw Slack attachment text when blocks are absent', async () => {
    const { fetchFn, requests } = fakeApi()
    const message = apiMessage('', {
      raw: {
        attachments: [
          {
            fallback: 'Monitor alert fired',
            fields: [
              { title: 'Service', value: 'centaur-api-rs' },
              { title: 'Cluster', value: 'prd-centaur-na' }
            ],
            title: 'High error rate'
          }
        ]
      }
    })

    await forwardToSessionApi(options(fetchFn), forwardInput(message))

    const expected = 'Monitor alert fired\nHigh error rate\nService\ncentaur-api-rs\nCluster\nprd-centaur-na'
    expect(appendedTextParts(requests)).toContain(expected)
    const line = executeLine(requests)
    expect(line.trace_metadata).toEqual(
      expect.objectContaining({
        slack_raw_attachment_count: 1,
        slack_text_source: 'raw_attachments'
      })
    )
    expect(lineContent(line).at(-1)).toEqual({ type: 'text', text: expected })
  })

  test('forwards hidden Slack message links with non-empty adapter text', async () => {
    const { fetchFn, requests } = fakeApi()
    const slackUrl = 'https://acme.slack.com/archives/C1234567890/p1700000000000100'
    const serialized = await serializeMessage({
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
      links: [],
      metadata: { dateSent: new Date('2026-06-10T00:00:00.000Z') },
      raw: {
        blocks: [
          {
            elements: [
              {
                elements: [
                  { text: 'continue', type: 'text' },
                  { text: 'source thread', type: 'link', url: slackUrl }
                ],
                type: 'rich_text_section'
              }
            ],
            type: 'rich_text'
          }
        ],
        team_id: 'T1'
      },
      text: 'continue',
      threadId: 'slack:C1:1700000000.000100'
    } as unknown as Parameters<typeof serializeMessage>[0])

    await forwardToSessionApi(options(fetchFn), forwardInput(serialized))

    expect(serialized.links).toEqual([{ isSlackMessage: true, url: slackUrl }])
    const expected = [
      'continue',
      '',
      'Links included in the Slack message:',
      'If the request is context-dependent, inspect linked Slack message/thread links before responding.',
      `- Slack message/thread: ${slackUrl}`
    ].join('\n')
    expect(appendedTextParts(requests)).toContain(expected)

    const line = executeLine(requests)
    expect(line.trace_metadata).toEqual(
      expect.objectContaining({
        slack_link_count: 1,
        slack_text_source: 'text'
      })
    )
    expect(lineContent(line).at(-1)).toEqual({ type: 'text', text: expected })
  })

  test('does not duplicate links already visible in Slack text', async () => {
    const { fetchFn, requests } = fakeApi()
    const slackUrl = 'https://acme.slack.com/archives/C1234567890/p1700000000000100'
    const message = apiMessage(`continue (${slackUrl})`, {
      links: [{ isSlackMessage: true, url: slackUrl }]
    })

    await forwardToSessionApi(options(fetchFn), forwardInput(message))

    expect(appendedTextParts(requests)).toContain(`continue (${slackUrl})`)
    expect(appendedTextParts(requests).join('\n')).not.toContain('Links included in the Slack message')
  })
})

describe('Slack attachment serialization', () => {
  test('records timeout errors when attachment fetchData hangs', async () => {
    const fetchFn = (async () => Response.json({ ok: true })) as SlackbotV2Options['fetch']
    const startedAt = Date.now()
    const attachment = await serializeAttachment(
      {
        fetchData: () => new Promise<Buffer>(() => undefined),
        name: 'hung.txt',
        type: 'file'
      } as Parameters<typeof serializeAttachment>[0],
      { ...options(fetchFn), slackApiTimeoutMs: 25 }
    )

    expect(Date.now() - startedAt).toBeLessThan(500)
    expect(attachment.fetchError).toBe('fetch Slack attachment timed out after 25ms')
  })
})

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
    expect(lineContent(line).some(part => textPartIncludes(part, '# Requester Context'))).toBe(true)
    expect(line.message.content.at(-1)).toEqual({ type: 'text', text: 'review this' })
  })

  test('omits model field when no override is set', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    expect('model' in line).toBe(false)
  })

  test('includes provider override on the execute input line', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('review this'), {
        model: 'custom-model',
        provider: 'responses'
      })
    )
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    expect(line.model).toBe('custom-model')
    expect(line.provider).toBe('responses')
  })

  test('omits provider field when no override is set', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    expect('provider' in line).toBe(false)
  })

  test('includes reasoning override on the execute input line', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('audit this'), { reasoning: 'high' })
    )
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    expect(line.reasoning).toBe('high')
  })

  test('omits reasoning field when no override is set', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    expect('reasoning' in line).toBe(false)
  })

  test('includes default idle timeout on execute requests', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    expect(executeBody(requests).idle_timeout_ms).toBe(DEFAULT_SESSION_IDLE_TIMEOUT_MS)
  })

  test('caps default idle timeout to max duration on execute requests', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      { ...options(fetchFn), maxDurationMs: 60_000 },
      forwardInput(apiMessage('hi'))
    )
    expect(executeBody(requests).idle_timeout_ms).toBe(60_000)
    expect(executeBody(requests).max_duration_ms).toBe(60_000)
  })

  test('allows idle timeout override on execute requests', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      { ...options(fetchFn), idleTimeoutMs: 12_345 },
      forwardInput(apiMessage('hi'))
    )
    expect(executeBody(requests).idle_timeout_ms).toBe(12_345)
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

  test('times out when session creation never settles', async () => {
    const fetchFn = (() => new Promise<Response>(() => undefined)) as SlackbotV2Options['fetch']
    await expect(
      forwardToSessionApi(
        { ...options(fetchFn), sessionApiTimeoutMs: 25 },
        forwardInput(apiMessage('hi'))
      )
    ).rejects.toThrow('create session timed out after 25ms')
  })
})

describe('forwardToSessionApi harness restart', () => {
  test('explicit harness override requests restart on conflict', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput(apiMessage('switch me'), { harnessType: 'codex' })
    )
    const create = requests.find(request => request.url.endsWith('.000100'))
    expect((create?.body as { on_harness_conflict?: string }).on_harness_conflict).toBe('restart')
  })

  test('default create does not request restart', async () => {
    const { fetchFn, requests } = fakeApi()
    await forwardToSessionApi(options(fetchFn), forwardInput(apiMessage('hi')))
    const create = requests.find(request => request.url.endsWith('.000100'))
    expect('on_harness_conflict' in (create?.body as object)).toBe(false)
  })

  test('harness_switched response fires onSessionRestarted and prepends the preamble', async () => {
    const { fetchFn, requests } = fakeApi({
      createSession: [{ body: { ok: true, harness_switched: true }, status: 200 }]
    })
    const message = apiMessage('continue with codex')
    const input = forwardInput(message, { harnessType: 'codex' })
    let restarted = false
    await forwardToSessionApi(options(fetchFn), input, {
      onSessionRestarted: async () => {
        restarted = true
        input.contextPreamble = harnessRestartPreamble(
          [
            { ...message, id: '1700000000.000001', text: 'earlier question' },
            {
              ...message,
              author: { ...message.author, isMe: true, userName: 'centaur' },
              id: '1700000000.000002',
              text: 'earlier answer'
            },
            message
          ],
          message.id
        )
      }
    })
    expect(restarted).toBe(true)
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    const content = lineContent(line)
    const requesterContext = content.find(part => textPartIncludes(part, '# Requester Context'))
    const preamble = content.find(part =>
      textPartIncludes(part, 'restarted on a different agent harness')
    )
    const current = content.at(-1)
    expect(requesterContext?.type).toBe('text')
    expect(requesterContext?.text).toContain('# Requester Context')
    expect(requesterContext?.text).not.toContain('restarted on a different agent harness')
    expect(preamble?.type).toBe('text')
    expect(preamble?.text).toContain('restarted on a different agent harness')
    expect(preamble?.text).toContain('[test]: earlier question')
    expect(preamble?.text).toContain('[assistant]: earlier answer')
    expect(preamble?.text).not.toContain('continue with codex')
    expect(current).toEqual({ type: 'text', text: 'continue with codex' })
  })

  test('no restart leaves the execute line without a preamble', async () => {
    const { fetchFn, requests } = fakeApi()
    const input = forwardInput(apiMessage('plain message'), { harnessType: 'codex' })
    let restarted = false
    await forwardToSessionApi(options(fetchFn), input, {
      onSessionRestarted: async () => {
        restarted = true
      }
    })
    expect(restarted).toBe(false)
    const execute = requests.find(request => request.url.endsWith('/execute'))
    const line = JSON.parse((execute?.body as { input_lines: string[] }).input_lines[0]!)
    const requesterContext = lineContent(line).find(part =>
      textPartIncludes(part, '# Requester Context')
    )
    expect(requesterContext?.text).toContain('# Requester Context')
    expect(requesterContext?.text).not.toContain('restarted on a different agent harness')
    expect(line.message.content.at(-1)).toEqual({ type: 'text', text: 'plain message' })
  })
})

describe('harnessRestartPreamble', () => {
  const base = apiMessage('current')

  test('returns undefined when there is no prior history', () => {
    expect(harnessRestartPreamble([base], base.id)).toBeUndefined()
  })

  test('truncates very long transcripts from the front', () => {
    const history = [
      { ...base, id: 'old.1', text: 'x'.repeat(30_000) },
      { ...base, id: 'old.2', text: 'most recent line' },
      base
    ]
    const preamble = harnessRestartPreamble(history, base.id)!
    expect(preamble).toContain('…(earlier messages truncated)')
    expect(preamble).toContain('most recent line')
    expect(preamble.length).toBeLessThan(26_000)
  })
})

describe('session principal display name', () => {
  function slackOptions(fetchFn: SlackbotV2Options['fetch']): SlackbotV2Options {
    return {
      apiUrl: 'http://api.test',
      botToken: 'xoxb-test',
      fetch: fetchFn,
      signingSecret: 'secret',
      // A slackApiUrl is required for the bot to make real Slack API calls
      // (channel/profile lookups); without it those lookups are skipped.
      slackApiUrl: 'http://slack.test/api/'
    }
  }

  function createBody(requests: RecordedRequest[]): {
    metadata?: {
      slack_conversation_name?: string
      slack_team_id?: string
      slack_user_id?: string
    }
  } {
    return (requests.find(request => request.url.endsWith('.000100'))?.body ?? {}) as {
      metadata?: {
        slack_conversation_name?: string
        slack_team_id?: string
        slack_user_id?: string
      }
    }
  }

  // slackApiGet uses the global fetch (not options.fetch), so Slack lookups are
  // stubbed by swapping globalThis.fetch for the duration of the run; the
  // session API itself still goes through the injected options.fetch.
  async function withSlackStub(
    stub: (url: string) => Promise<Response> | Response,
    run: () => Promise<void>
  ): Promise<void> {
    const realFetch = globalThis.fetch
    globalThis.fetch = (async (input: RequestInfo | URL) =>
      stub(String(input))) as typeof fetch
    clearConversationNameCacheForTests()
    clearRequesterIdentityCacheForTests()
    try {
      await run()
    } finally {
      globalThis.fetch = realFetch
    }
  }

  test('uses the GitHub handle from the requester Slack profile for PR attribution', async () => {
    const { fetchFn, requests } = fakeApi()
    await withSlackStub(
      url => {
        if (url.includes('conversations.info')) {
          return Response.json({ channel: { id: 'C1', name_normalized: 'eng-oncall' }, ok: true })
        }
        if (url.includes('users.profile.get')) {
          return Response.json({
            ok: true,
            profile: {
              display_name: 'Ada Lovelace',
              fields: {
                XfGithub: { label: 'GitHub', value: 'https://github.com/ada-lovelace' }
              },
              name: 'ada'
            }
          })
        }
        if (url.includes('users.info')) {
          return Response.json({ ok: true, user: { profile: { display_name: 'Ada Lovelace' } } })
        }
        return Response.json({ ok: true })
      },
      async () => {
        await forwardToSessionApi(slackOptions(fetchFn), forwardInput(apiMessage('please PR')))
      }
    )

    const requesterContext = lineContent(executeLine(requests)).find(part =>
      textPartIncludes(part, '# Requester Context')
    )
    expect(requesterContext?.text).toContain('GitHub handle from Slack profile: @ada-lovelace')
    expect(requesterContext?.text).toContain('Prompted by: @ada-lovelace')
    expect(requesterContext?.text).toContain('Assign the PR to the requester when possible: `ada-lovelace`')
  })

  test('uses the requester Slack display name for PR attribution when no GitHub handle exists', async () => {
    const { fetchFn, requests } = fakeApi()
    await withSlackStub(
      url => {
        if (url.includes('conversations.info')) {
          return Response.json({ channel: { id: 'C1', name_normalized: 'eng-oncall' }, ok: true })
        }
        if (url.includes('users.profile.get')) {
          return Response.json({
            ok: true,
            profile: {
              display_name: 'Ada Lovelace',
              fields: {},
              name: 'ada'
            }
          })
        }
        if (url.includes('users.info')) {
          return Response.json({ ok: true, user: { profile: { display_name: 'Ada Lovelace' } } })
        }
        return Response.json({ ok: true })
      },
      async () => {
        await forwardToSessionApi(slackOptions(fetchFn), forwardInput(apiMessage('please PR')))
      }
    )

    const requesterContext = lineContent(executeLine(requests)).find(part =>
      textPartIncludes(part, '# Requester Context')
    )
    expect(requesterContext?.text).toContain('GitHub handle from Slack profile: unavailable')
    expect(requesterContext?.text).toContain('Prompted by: Ada Lovelace')
    expect(requesterContext?.text).toContain(
      'Use the requester\'s Slack display name or username because no verified GitHub handle is available.'
    )
    expect(requesterContext?.text).not.toContain('Omit the `Prompted by` line')
  })

  test('channel sessions name the principal after the channel', async () => {
    const { fetchFn, requests } = fakeApi()
    await withSlackStub(
      url =>
        url.includes('conversations.info')
          ? Response.json({ channel: { id: 'C1', name_normalized: 'eng-oncall' }, ok: true })
          : Response.json({ ok: true }),
      async () => {
        await forwardToSessionApi(slackOptions(fetchFn), forwardInput(apiMessage('hi')))
      }
    )
    expect(createBody(requests).metadata?.slack_conversation_name).toBe('eng-oncall')
  })

  test('continues creating the session when the channel lookup never settles', async () => {
    const { fetchFn, requests } = fakeApi()
    let slackCalls = 0
    await withSlackStub(
      url => {
        if (url.includes('conversations.info')) {
          slackCalls += 1
          return new Promise<Response>(() => undefined)
        }
        return Response.json({ ok: true })
      },
      async () => {
        await forwardToSessionApi(
          { ...slackOptions(fetchFn), slackApiTimeoutMs: 25 },
          forwardInput(apiMessage('hi'))
        )
      }
    )
    expect(slackCalls).toBe(1)
    expect('slack_conversation_name' in (createBody(requests).metadata ?? {})).toBe(false)
    expect(requests.some(request => request.url.endsWith('/execute'))).toBe(true)
  })

  test('DM sessions name the principal after the DM partner', async () => {
    const { fetchFn, requests } = fakeApi()
    const dm = apiMessage('hi')
    dm.threadId = 'slack:D9:1700000000.000100'
    dm.raw = { channel: 'D9' }
    await withSlackStub(
      url =>
        url.includes('users.info')
          ? Response.json({ ok: true, user: { profile: { display_name: 'Ada Lovelace' } } })
          : Response.json({ ok: true }),
      async () => {
        await forwardToSessionApi(slackOptions(fetchFn), forwardInput(dm))
      }
    )
    expect(createBody(requests).metadata?.slack_conversation_name).toBe('Ada Lovelace')
    expect(createBody(requests).metadata?.slack_team_id).toBe('T1')
    expect(createBody(requests).metadata?.slack_user_id).toBe('U1')
  })

  test('falls back to no name when the channel lookup fails', async () => {
    const { fetchFn, requests } = fakeApi()
    await withSlackStub(
      url =>
        url.includes('conversations.info')
          ? Response.json({ error: 'channel_not_found', ok: false })
          : Response.json({ ok: true }),
      async () => {
        await forwardToSessionApi(slackOptions(fetchFn), forwardInput(apiMessage('hi')))
      }
    )
    expect('slack_conversation_name' in (createBody(requests).metadata ?? {})).toBe(false)
  })
})
