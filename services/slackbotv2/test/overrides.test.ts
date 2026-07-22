import { describe, expect, test } from 'bun:test'
import { SlackFormatConverter } from '@chat-adapter/slack'
import {
  extractMessageOverrides,
  normalizeHarnessOverrides,
  validateStrategyOverrides
} from '../src/overrides'
import { messageOverridesForText } from '../src/index'
import { createOpenAiMessageOverridesStrategy } from '../src/message-overrides-strategy'
import type { SlackbotV2Options, SlackbotV2Trace } from '../src/types'

describe('extractMessageOverrides', () => {
  test('returns text untouched without flags', () => {
    const result = extractMessageOverrides('review this PR --not-a-known-flag stays')
    expect(result).toEqual({
      cleanedText: 'review this PR --not-a-known-flag stays',
      harnessType: undefined,
      model: undefined,
      reasoning: undefined
    })
  })

  test('parses harness flags', () => {
    expect(extractMessageOverrides('--claude review this')).toEqual({
      cleanedText: 'review this',
      harnessType: 'claudecode',
      model: undefined,
      reasoning: undefined
    })
    expect(extractMessageOverrides('--claude-code review this').harnessType).toBe('claudecode')
    expect(extractMessageOverrides('--amp review this').harnessType).toBe('amp')
    expect(extractMessageOverrides('--codex review this').harnessType).toBe('codex')
    expect(extractMessageOverrides('--nanocodex review this').harnessType).toBe('nanocodex')
  })

  test('parses harness flag anywhere in the message', () => {
    expect(extractMessageOverrides('review this --amp please')).toEqual({
      cleanedText: 'review this please',
      harnessType: 'amp',
      model: undefined,
      reasoning: undefined
    })
  })

  test('is case-insensitive', () => {
    expect(extractMessageOverrides('--Claude review').harnessType).toBe('claudecode')
  })

  test('parses --model with space or equals', () => {
    expect(extractMessageOverrides('--claude --model claude-sonnet-4-6 fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'claudecode',
      model: 'claude-sonnet-4-6',
      reasoning: undefined
    })
    expect(extractMessageOverrides('--codex --model=gpt-5.2 fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: 'gpt-5.2',
      reasoning: undefined
    })
  })

  test('model shortcuts set model and imply claude-code', () => {
    expect(extractMessageOverrides('--opus fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'claudecode',
      model: 'claude-opus-4-8',
      reasoning: undefined
    })
    expect(extractMessageOverrides('--sonnet fix it').model).toBe('claude-sonnet-4-6')
    expect(extractMessageOverrides('--haiku fix it').model).toBe('claude-haiku-4-5')
    expect(extractMessageOverrides('--fable fix it').model).toBe('claude-fable-5')
  })

  test('--meta selects the Meta provider and codex harness', () => {
    expect(extractMessageOverrides('--meta fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: undefined,
      provider: 'responses',
      reasoning: undefined
    })
  })

  test('--model expands claude aliases to full model ids', () => {
    expect(extractMessageOverrides('--claude --model opus go')).toEqual({
      cleanedText: 'go',
      harnessType: 'claudecode',
      model: 'claude-opus-4-8'
    })
    expect(extractMessageOverrides('--model Sonnet go').model).toBe('claude-sonnet-4-6')
    expect(extractMessageOverrides('--model fable go').model).toBe('claude-fable-5')
  })

  test('--model accepts a newline immediately after the value', () => {
    expect(extractMessageOverrides('--claude --model=fable\nwhat model are you')).toEqual({
      cleanedText: 'what model are you',
      harnessType: 'claudecode',
      model: 'claude-fable-5',
      reasoning: undefined
    })
    expect(
      extractMessageOverrides('@Centaur AI --claude --model=fable\r\nwhat model are you')
    ).toEqual({
      cleanedText: '@Centaur AI what model are you',
      harnessType: 'claudecode',
      model: 'claude-fable-5',
      reasoning: undefined
    })
  })

  test('--model accepts a rendered line break immediately after the value', () => {
    expect(extractMessageOverrides('--claude --model=fable<br>what model are you')).toEqual({
      cleanedText: 'what model are you',
      harnessType: 'claudecode',
      model: 'claude-fable-5',
      reasoning: undefined
    })
  })

  test('--model passes non-alias values through verbatim', () => {
    expect(extractMessageOverrides('--codex --model gpt-5.2-codex go').model).toBe('gpt-5.2-codex')
    expect(extractMessageOverrides('--amp --model fast go').model).toBe('fast')
  })

  test('explicit flags win over shortcut implications', () => {
    expect(extractMessageOverrides('--codex --opus fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: 'claude-opus-4-8',
      reasoning: undefined
    })
    expect(extractMessageOverrides('--sonnet --model claude-opus-4-8 fix it').model).toBe(
      'claude-opus-4-8'
    )
  })

  test('does not match flags embedded in words or longer flags', () => {
    expect(extractMessageOverrides('run pre--claude task').harnessType).toBeUndefined()
    expect(extractMessageOverrides('--claudette hi').harnessType).toBeUndefined()
    expect(extractMessageOverrides('--ampere hi').harnessType).toBeUndefined()
  })

  test('flag-only message cleans to empty text', () => {
    expect(extractMessageOverrides('--claude')).toEqual({
      cleanedText: '',
      harnessType: 'claudecode',
      model: undefined,
      reasoning: undefined
    })
  })

  test('--model without a value is left untouched', () => {
    expect(extractMessageOverrides('what does --model do?')).toEqual({
      cleanedText: 'what does --model do?',
      harnessType: undefined,
      model: undefined,
      reasoning: undefined
    })
    expect(extractMessageOverrides('--model\nwhat model are you')).toEqual({
      cleanedText: '--model\nwhat model are you',
      harnessType: undefined,
      model: undefined,
      reasoning: undefined
    })
  })

  test('parses -rsn with space or equals', () => {
    expect(extractMessageOverrides('-rsn high fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: undefined,
      model: undefined,
      reasoning: 'high'
    })
    expect(extractMessageOverrides('-rsn=medium fix it').reasoning).toBe('medium')
  })

  test('-rsn is case-insensitive and normalizes the effort value', () => {
    expect(extractMessageOverrides('-rsn HIGH fix it').reasoning).toBe('high')
    expect(extractMessageOverrides('-rsn Medium fix it').reasoning).toBe('medium')
  })

  test('-rsn accepts short aliases', () => {
    expect(extractMessageOverrides('-rsn min fix it').reasoning).toBe('minimal')
    expect(extractMessageOverrides('-rsn med fix it').reasoning).toBe('medium')
    expect(extractMessageOverrides('-rsn hi fix it').reasoning).toBe('high')
    expect(extractMessageOverrides('-rsn xhi fix it').reasoning).toBe('xhigh')
  })

  test('-rsn accepts the GPT-5.6 max effort', () => {
    expect(extractMessageOverrides('-rsn max fix it').reasoning).toBe('max')
  })

  test('-rsn combines with a harness flag', () => {
    expect(extractMessageOverrides('-rsn high --codex audit this')).toEqual({
      cleanedText: 'audit this',
      harnessType: 'codex',
      model: undefined,
      reasoning: 'high'
    })
  })

  test('-rsn with an unknown effort value is left untouched', () => {
    expect(extractMessageOverrides('-rsn turbo fix it')).toEqual({
      cleanedText: '-rsn turbo fix it',
      harnessType: undefined,
      model: undefined,
      reasoning: undefined
    })
  })

  test('-rsn without a value is left untouched', () => {
    expect(extractMessageOverrides('what does -rsn do?')).toEqual({
      cleanedText: 'what does -rsn do?',
      harnessType: undefined,
      model: undefined,
      reasoning: undefined
    })
  })

  test('--bedrock selects the bedrock provider and implies codex', () => {
    expect(extractMessageOverrides('--bedrock fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: undefined,
      provider: 'amazon-bedrock',
      reasoning: undefined
    })
  })

  test('--bedrock combines with an explicit --model', () => {
    expect(
      extractMessageOverrides('--bedrock --model anthropic.claude-sonnet-4-5 fix it')
    ).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: 'anthropic.claude-sonnet-4-5',
      provider: 'amazon-bedrock',
      reasoning: undefined
    })
  })

  test('--bedrock does not match flags embedded in words', () => {
    expect(extractMessageOverrides('--bedrocky hi').provider).toBeUndefined()
    expect(extractMessageOverrides('the --bedrock flag').provider).toBe('amazon-bedrock')
  })

  test('--meta combines with a reasoning override', () => {
    expect(extractMessageOverrides('--meta -rsn high fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: undefined,
      provider: 'responses',
      reasoning: 'high'
    })
  })
})

// normalizeHarnessOverrides is the object-shaped sibling of
// extractMessageOverrides: config fields resolve through the SAME vocabulary
// tables as the inline flags, so a channel default and a Slack flag validate
// identically.
describe('normalizeHarnessOverrides', () => {
  test('resolves harness / model / provider / reasoning through the flag vocabulary', () => {
    expect(
      normalizeHarnessOverrides({ harness: 'claude', model: 'opus', reasoning: 'hi' })
    ).toEqual({
      harnessType: 'claudecode',
      model: 'claude-opus-4-8',
      provider: undefined,
      reasoning: 'high'
    })
  })

  test('a provider shortcut implies its harness, like --bedrock', () => {
    expect(normalizeHarnessOverrides({ provider: 'bedrock', model: 'gpt-5.2' })).toEqual({
      harnessType: 'codex',
      model: 'gpt-5.2',
      provider: 'amazon-bedrock',
      reasoning: undefined
    })
  })

  test('expands a model alias but does not imply a harness (fields are independent)', () => {
    // Like `--model opus` (not `--opus`): the alias expands, harness is left to
    // the explicit `harness` field / thread / deployment default.
    expect(normalizeHarnessOverrides({ model: 'opus' })).toEqual({
      harnessType: undefined,
      model: 'claude-opus-4-8',
      provider: undefined,
      reasoning: undefined
    })
  })

  test('reports and drops unrecognized enum-like values', () => {
    const errors: string[] = []
    const result = normalizeHarnessOverrides(
      { harness: 'gpt', provider: 'openai', reasoning: 'turbo' },
      message => errors.push(message)
    )
    expect(result).toEqual({
      harnessType: undefined,
      model: undefined,
      provider: undefined,
      reasoning: undefined
    })
    expect(errors.some(e => e.includes('unknown harness'))).toBe(true)
    expect(errors.some(e => e.includes('unknown provider'))).toBe(true)
    expect(errors.some(e => e.includes('unknown reasoning effort'))).toBe(true)
  })
})

describe('validateStrategyOverrides', () => {
  test('accepts canonical strategy model ids', () => {
    expect(
      validateStrategyOverrides({
        model: 'gpt-5.6-sol',
        reasoning: 'max'
      })
    ).toEqual({
      harnessType: 'codex',
      model: 'gpt-5.6-sol',
      provider: undefined,
      reasoning: 'max'
    })
  })

  test('accepts canonical OpenAI model ids from the model catalog', () => {
    expect(validateStrategyOverrides({ model: 'gpt-5.6-terra' })).toEqual({
      harnessType: 'codex',
      model: 'gpt-5.6-terra',
      provider: undefined,
      reasoning: undefined
    })
    expect(validateStrategyOverrides({ model: 'gpt-5.6-luna' })).toEqual({
      harnessType: 'codex',
      model: 'gpt-5.6-luna',
      provider: undefined,
      reasoning: undefined
    })
    expect(validateStrategyOverrides({ model: 'gpt-5.5-pro' })).toEqual({
      harnessType: 'codex',
      model: 'gpt-5.5-pro',
      provider: undefined,
      reasoning: undefined
    })
  })

  test('canonical strategy model ids imply their compatible harness', () => {
    expect(validateStrategyOverrides({ model: 'claude-opus-4-7' })).toEqual({
      harnessType: 'claudecode',
      model: 'claude-opus-4-7',
      provider: undefined,
      reasoning: undefined
    })
    expect(
      validateStrategyOverrides({
        model: 'claude-opus-4-8'
      })
    ).toEqual({
      harnessType: 'claudecode',
      model: 'claude-opus-4-8',
      provider: undefined,
      reasoning: undefined
    })
    expect(validateStrategyOverrides({ model: 'claude-sonnet-4-6' })).toEqual({
      harnessType: 'claudecode',
      model: 'claude-sonnet-4-6',
      provider: undefined,
      reasoning: undefined
    })
    expect(validateStrategyOverrides({ model: 'claude-sonnet-5' })).toEqual({
      harnessType: 'claudecode',
      model: 'claude-sonnet-5',
      provider: undefined,
      reasoning: undefined
    })
  })

  test('rejects aliases and arbitrary model ids from the strategy path', () => {
    expect(validateStrategyOverrides({ model: 'terra' })).toEqual({})
    expect(validateStrategyOverrides({ model: 'anthropic/claude-fable-5' })).toEqual({})
    expect(validateStrategyOverrides({ model: 'not real model id' })).toEqual({})
  })

  test('rejects incompatible canonical strategy fields', () => {
    expect(validateStrategyOverrides({ harness: 'codex', model: 'claude-opus-4-8' })).toEqual({})
    expect(validateStrategyOverrides({ harness: 'amp', provider: 'responses' })).toEqual({})
    expect(validateStrategyOverrides({ reasoning: 'turbo' })).toEqual({})
  })

  test('drops reasoning when the resolved strategy harness cannot use it', () => {
    expect(validateStrategyOverrides({ reasoning: 'max' })).toEqual({
      harnessType: undefined,
      model: undefined,
      provider: undefined,
      reasoning: 'max'
    })
    expect(validateStrategyOverrides({ model: 'claude-opus-4-8', reasoning: 'max' })).toEqual({
      harnessType: 'claudecode',
      model: 'claude-opus-4-8',
      provider: undefined,
      reasoning: undefined
    })
    expect(validateStrategyOverrides({ harness: 'amp', reasoning: 'max' })).toEqual({
      harnessType: 'amp',
      model: undefined,
      provider: undefined,
      reasoning: undefined
    })
    expect(validateStrategyOverrides({ model: 'gpt-5.6-sol', reasoning: 'max' })).toEqual({
      harnessType: 'codex',
      model: 'gpt-5.6-sol',
      provider: undefined,
      reasoning: 'max'
    })
  })
})

describe('messageOverridesForText strategy invocation', () => {
  const trace: SlackbotV2Trace = {
    includeContext: false,
    messageId: 'm1',
    mode: 'execute',
    openStream: false,
    startedAtMs: 0,
    threadId: 'slack:C1:1'
  }

  test('uses the flags strategy by default', async () => {
    await expect(
      messageOverridesForText(slackOptions({}), '--opus fix it', trace)
    ).resolves.toEqual({
      cleanedText: 'fix it',
      overrides: {
        harnessType: 'claudecode',
        model: 'claude-opus-4-8',
        provider: undefined,
        reasoning: undefined
      }
    })
  })

  test('uses the configured strategy instead of the legacy flag parser', async () => {
    await expect(
      messageOverridesForText(
        slackOptions({
          messageOverridesStrategy: async () => ({ overrides: {} })
        }),
        '--opus fix it',
        trace
      )
    ).resolves.toEqual({ overrides: {} })
  })

  test('returns configured strategy overrides without cleaning prompt text', async () => {
    await expect(
      messageOverridesForText(
        slackOptions({
          messageOverridesStrategy: async () => ({
            overrides: {
              harnessType: 'codex',
              model: 'gpt-5.6-sol',
              provider: undefined,
              reasoning: 'max'
            }
          })
        }),
        'do the work. use max effort and the sol model.',
        trace
      )
    ).resolves.toEqual({
      overrides: {
        harnessType: 'codex',
        model: 'gpt-5.6-sol',
        provider: undefined,
        reasoning: 'max'
      }
    })
  })

  test('falls back when the OpenAI strategy request fails', async () => {
    await expect(
      messageOverridesForText(
        slackOptions({
          messageOverridesStrategy: createOpenAiMessageOverridesStrategy({
            apiKey: 'test-key',
            fetch: (async () =>
              new Response('secret-token=do-not-log', {
                status: 503,
                statusText: 'Service Unavailable'
              })) as unknown as typeof fetch,
            model: 'gpt-5.4-nano'
          })
        }),
        'use sol',
        trace
      )
    ).resolves.toEqual({ overrides: {} })
  })

  test('handles --nanocodex deterministically before the OpenAI strategy', async () => {
    let requestCount = 0
    const strategy = createOpenAiMessageOverridesStrategy({
      apiKey: 'test-key',
      fetch: (async () => {
        requestCount += 1
        throw new Error('the explicit flag must not call the strategy model')
      }) as unknown as typeof fetch,
      model: 'gpt-5.4-nano'
    })

    await expect(strategy({ text: '--nanocodex review this' })).resolves.toEqual({
      cleanedText: 'review this',
      overrides: {
        harnessType: 'nanocodex',
        model: undefined,
        provider: undefined,
        reasoning: undefined
      }
    })
    expect(requestCount).toBe(0)
  })

  test('allows the OpenAI strategy to select nanocodex from natural language', async () => {
    let requestBody: Record<string, unknown> | undefined
    const strategy = createOpenAiMessageOverridesStrategy({
      apiKey: 'test-key',
      fetch: (async (_input: RequestInfo | URL, init?: RequestInit) => {
        requestBody = JSON.parse(String(init?.body)) as Record<string, unknown>
        return Response.json({
          output: [
            {
              content: [
                {
                  text: JSON.stringify({
                    harness: 'nanocodex',
                    model: null,
                    provider: null,
                    reasoning: null
                  })
                }
              ]
            }
          ]
        })
      }) as unknown as typeof fetch,
      model: 'gpt-5.4-nano'
    })

    await expect(strategy({ text: 'use nanocodex for this' })).resolves.toEqual({
      overrides: {
        harnessType: 'nanocodex',
        model: undefined,
        provider: undefined,
        reasoning: undefined
      }
    })
    expect(JSON.stringify(requestBody)).toContain('nanocodex')
  })
})

function slackOptions(overrides: Partial<SlackbotV2Options>): SlackbotV2Options {
  return {
    apiUrl: 'http://api.example.test',
    botToken: 'xoxb-test',
    signingSecret: 'secret',
    ...overrides
  }
}

// The adapter's plain-text extraction feeds extractMessageOverrides. The
// unpatched @chat-adapter/slack flattened the parsed AST with
// mdast-util-to-string, which joins sibling paragraphs with NO separator —
// `--model=fable\n\nexamine ...` reached the parser as `--model=fableexamine
// ...` and the harness got a nonexistent model. The patched converter
// preserves block boundaries; these tests exercise the real pipeline.
describe('SlackFormatConverter.extractPlainText + extractMessageOverrides', () => {
  const converter = new SlackFormatConverter()

  test('paragraph break after --model survives plain-text extraction', () => {
    const mrkdwn =
      '--claude --model=fable\n\nexamine <https://github.com/paradigmxyz/centaur/pull/921|github.com/paradigmxyz/centaur/pull/921>. cross reference that PR.'
    const text = converter.extractPlainText(mrkdwn)
    expect(text).toBe(
      '--claude --model=fable\n\nexamine github.com/paradigmxyz/centaur/pull/921. cross reference that PR.'
    )
    expect(extractMessageOverrides(text)).toEqual({
      cleanedText: 'examine github.com/paradigmxyz/centaur/pull/921. cross reference that PR.',
      harnessType: 'claudecode',
      model: 'claude-fable-5',
      reasoning: undefined
    })
  })

  test('single newlines and paragraph breaks are both preserved', () => {
    expect(converter.extractPlainText('--model=fable\nexamine this')).toBe(
      '--model=fable\nexamine this'
    )
    expect(converter.extractPlainText('line1\n\nline2\nline3')).toBe('line1\n\nline2\nline3')
  })

  test('list items and blockquotes keep line boundaries', () => {
    expect(converter.extractPlainText('- item1\n- item2\n\nafter list')).toBe(
      'item1\nitem2\n\nafter list'
    )
    expect(converter.extractPlainText('> quoted line\n\nafter quote')).toBe(
      'quoted line\n\nafter quote'
    )
  })
})
