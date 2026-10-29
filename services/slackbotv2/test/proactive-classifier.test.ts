import { describe, expect, test } from 'bun:test'
import { createOpenAiProactiveClassifier } from '../src/proactive-classifier'
import type { ProactiveClassifierInput } from '../src/types'

const input: ProactiveClassifierInput = {
  botName: 'centaur',
  history: [
    { author: 'Test User', fromBot: false, text: '@centaur why did the deploy fail?' },
    { author: 'centaur', fromBot: true, text: 'The migration timed out.' }
  ],
  message: { author: 'Test User', fromBot: false, text: 'can you retry it?' }
}

function classifierReturning(response: () => Response, requests: unknown[] = []) {
  return createOpenAiProactiveClassifier({
    apiKey: 'test-key',
    baseUrl: 'https://llm.example.test/v1/',
    fetch: (async (url: RequestInfo | URL, init?: RequestInit) => {
      requests.push({ body: JSON.parse(String(init?.body)), url: String(url) })
      return response()
    }) as unknown as typeof fetch,
    model: 'gpt-6-luna'
  })
}

describe('OpenAI proactive classifier', () => {
  test('asks a respond predicate over the labelled transcript', async () => {
    const requests: Array<{ body: Record<string, unknown>; url: string }> = []
    const classifier = classifierReturning(
      () => Response.json({ answers: [{ name: 'respond', probability: 0.82, type: 'predicate' }] }),
      requests
    )

    await expect(classifier(input, new AbortController().signal)).resolves.toEqual({
      probability: 0.82,
      respond: true
    })
    expect(requests).toHaveLength(1)
    expect(requests[0]!.url).toBe('https://llm.example.test/v1/decisions')
    expect(requests[0]!.body).toMatchObject({
      model: 'gpt-6-luna',
      questions: [{ name: 'respond', type: 'predicate' }]
    })
    const messages = requests[0]!.body.input as Array<{ content: string; role: string }>
    expect(messages.map(message => message.role)).toEqual(['developer', 'user'])
    expect(messages[1]!.content).toBe([
      'Latest thread messages, oldest first:',
      '[Test User]: @centaur why did the deploy fail?',
      '[centaur (assistant)]: The migration timed out.',
      '',
      'Newest message:',
      '[Test User]: can you retry it?'
    ].join('\n'))
  })

  test('picks the first rule, in order, that the message meets', async () => {
    const requests: Array<{ body: Record<string, unknown>; url: string }> = []
    let probabilities = [0.3, 0.7, 0.95]
    const classifier = classifierReturning(
      () => Response.json({
        answers: probabilities.map((probability, index) => ({
          name: `rule_${index + 1}`,
          probability,
          type: 'predicate'
        }))
      }),
      requests
    )
    const rules = [
      { name: 'incident', when: 'An active incident.' },
      { context: 'Link the runbook.', name: 'deploy', when: 'Questions about deploys.' },
      { name: 'anything', when: 'Any question.' }
    ]

    await expect(classifier({ ...input, rules }, new AbortController().signal)).resolves.toEqual({
      probability: 0.7,
      respond: true,
      rule: 'deploy'
    })
    probabilities = [0.3, 0.4, 0.1]
    await expect(classifier({ ...input, rules }, new AbortController().signal)).resolves.toEqual({
      probability: 0.4,
      respond: false
    })

    expect(requests).toHaveLength(2)
    expect(requests[0]!.body.questions).toMatchObject([
      { name: 'rule_1', type: 'predicate' },
      { name: 'rule_2', type: 'predicate' },
      { name: 'rule_3', type: 'predicate' }
    ])
    const messages = requests[0]!.body.input as Array<{ content: string; role: string }>
    expect(messages[0]!.content).toContain('Rule 2 (deploy):\nQuestions about deploys.')
    expect(messages[0]!.content).not.toContain('was mentioned earlier')
    // Rule context is for the agent, not the classifier.
    expect(messages[0]!.content).not.toContain('Link the runbook.')
    expect(messages[1]!.content).not.toContain('Questions about deploys.')
  })

  test('declines below the probability threshold', async () => {
    const classifier = classifierReturning(() =>
      Response.json({ answers: [{ name: 'respond', probability: 0.2, type: 'predicate' }] })
    )
    await expect(classifier(input, new AbortController().signal)).resolves.toEqual({
      probability: 0.2,
      respond: false
    })
  })

  test('rejects failed requests and answers without a probability', async () => {
    const failing = classifierReturning(
      () => new Response('unavailable', { status: 503, statusText: 'Service Unavailable' })
    )
    await expect(failing(input, new AbortController().signal)).rejects.toThrow('HTTP 503')

    const malformed = classifierReturning(() => Response.json({ answers: [] }))
    await expect(malformed(input, new AbortController().signal)).rejects.toThrow('probability')
    await expect(
      malformed({ ...input, rules: [{ name: 'any', when: 'Any question.' }] }, new AbortController().signal)
    ).rejects.toThrow('rule_1 probability')
  })
})
