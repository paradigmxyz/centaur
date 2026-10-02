import { describe, expect, test } from 'bun:test'
import { createJevAmbientTriggerStrategy } from '../src/ambient-trigger-strategy'

const CHANNEL_ID = 'C0B4ZDRQ6MC'
const MESSAGES = [
  { author: 'centaur' as const, current: false, text: 'I can check that deployment.' },
  { author: 'user' as const, current: true, text: 'please do' }
]

describe('createJevAmbientTriggerStrategy', () => {
  test('starts a response only when the Jev probability reaches the threshold', async () => {
    const requests: Array<{ input: RequestInfo | URL; init?: RequestInit }> = []
    const strategy = createJevAmbientTriggerStrategy({
      apiKey: 'openrouter-key',
      fetch: async (input, init) => {
        requests.push({ input, init })
        return Response.json({
          answers: { should_respond: { noul: 0.93, type: 'noul' } },
          model: 'typesafe/jev-1.13-20260917',
          usage: { cost: 0.000012, input_tokens: 300, output_tokens: 20 }
        })
      },
      instructions: 'Use the deployment-specific response policy.',
      threshold: 0.9
    })

    await expect(strategy({ channelId: CHANNEL_ID, isThreadReply: false, messages: MESSAGES })).resolves.toEqual({
      model: 'typesafe/jev-1.13-20260917',
      probability: 0.93,
      respond: true,
      usage: { costUsd: 0.000012, inputTokens: 300 }
    })
    expect(requests).toHaveLength(1)
    expect(String(requests[0]!.input)).toBe('https://openrouter.ai/api/alpha/decisions')
    expect(requests[0]!.init?.headers).toEqual({
      authorization: 'Bearer openrouter-key',
      'content-type': 'application/json'
    })
    const body = JSON.parse(String(requests[0]!.init?.body)) as Record<string, unknown>
    expect(body.model).toBe('~typesafe/jev-latest')
    expect(body.state).toEqual({ channel_id: CHANNEL_ID, is_thread_reply: false, messages: MESSAGES })
    expect(body.questions).toEqual({
      should_respond: expect.objectContaining({
        instructions: 'Use the deployment-specific response policy.',
        type: 'noul'
      })
    })
  })

  test('keeps a low-probability message silent', async () => {
    const strategy = createJevAmbientTriggerStrategy({
      apiKey: 'openrouter-key',
      fetch: async () => Response.json({
        answers: { should_respond: { noul: 0.89, type: 'noul' } }
      }),
      threshold: 0.9
    })

    await expect(strategy({ channelId: CHANNEL_ID, isThreadReply: true, messages: MESSAGES })).resolves.toEqual({
      model: undefined,
      probability: 0.89,
      respond: false,
      usage: undefined
    })
  })

  test('fails closed when Jev returns an invalid decision', async () => {
    const strategy = createJevAmbientTriggerStrategy({
      apiKey: 'openrouter-key',
      fetch: async () => Response.json({ answers: {} })
    })

    await expect(strategy({ channelId: CHANNEL_ID, isThreadReply: false, messages: MESSAGES })).rejects.toThrow(
      'did not include a valid should_respond probability'
    )
  })

  test('aborts a decision that exceeds its deadline', async () => {
    const strategy = createJevAmbientTriggerStrategy({
      apiKey: 'openrouter-key',
      fetch: async (_input, init) => new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => {
          reject(new DOMException('The operation was aborted', 'AbortError'))
        })
      }),
      timeoutMs: 5
    })

    await expect(strategy({ channelId: CHANNEL_ID, isThreadReply: false, messages: MESSAGES })).rejects.toMatchObject({ name: 'AbortError' })
  })
})
