import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { configuredModels, findConfiguredModel } from '../src/model-catalog'
import { extractMessageOverrides, modelSelectionNames, namesRequestedSelection, normalizeHarnessOverrides, validateStrategyOverrides } from '../src/overrides'
import { createOpenAiMessageOverridesStrategy } from '../src/message-overrides-strategy'
import { buildSlackResponseContextBlock, reasoningForModel } from '../src/response-context'

const model = {
  displayName: 'Atlas Full', aliases: ['atlas'], description: 'Full Atlas model',
  reasoningEfforts: ['low', 'high', 'max'], defaultReasoning: 'max',
  reasoningAliases: { xhigh: 'max' }
}
const provider = { name: 'Private Inference', defaultModel: 'atlas-full', models: { 'atlas-full': model } }
let previous: string | undefined
beforeEach(() => {
  previous = process.env.CODEX_CUSTOM_PROVIDERS
  process.env.CODEX_CUSTOM_PROVIDERS = JSON.stringify({ private: provider })
})
afterEach(() => {
  if (previous === undefined) delete process.env.CODEX_CUSTOM_PROVIDERS
  else process.env.CODEX_CUSTOM_PROVIDERS = previous
})

describe('configured model selection', () => {
  test('routes flags, channel defaults and classifier selections through the same provider', () => {
    const expected = { harnessType: 'codex', model: 'atlas-full', provider: 'private' }
    expect(extractMessageOverrides('--model atlas review this')).toMatchObject({ ...expected, cleanedText: 'review this' })
    expect(normalizeHarnessOverrides({ model: 'atlas' })).toMatchObject(expected)
    expect(validateStrategyOverrides({ model: 'atlas-full' })).toMatchObject(expected)
    expect(validateStrategyOverrides({ provider: 'private' })).toMatchObject(expected)
    expect(validateStrategyOverrides({ model: 'atlas-full', provider: 'openrouter' })).toEqual({})
    expect(extractMessageOverrides('--claude --model atlas test').selectionError).toContain('incompatible')
  })

  test('does not guess between aliases registered by multiple providers', () => {
    process.env.CODEX_CUSTOM_PROVIDERS = JSON.stringify({ private: provider, other: provider })
    expect(findConfiguredModel('atlas')).toBeUndefined()
    expect(validateStrategyOverrides({ model: 'atlas-full' })).toEqual({})
    expect(validateStrategyOverrides({ model: 'atlas-full', provider: 'private' })).toMatchObject({ provider: 'private' })
    expect(validateStrategyOverrides({ model: 'private/atlas-full' })).toMatchObject({ model: 'atlas-full', provider: 'private' })
  })

  test('keeps minimal metadata backward compatible and ignores malformed JSON', () => {
    process.env.CODEX_CUSTOM_PROVIDERS = JSON.stringify({ private: { models: { m: { displayName: 'M' } } } })
    expect(findConfiguredModel('m')?.reasoningEfforts).toEqual([])
    process.env.CODEX_CUSTOM_PROVIDERS = '{'
    expect(configuredModels()).toEqual([])
  })

  test('only allows a named target to replace a sticky selection', () => {
    const selection = { model: 'atlas-full', provider: 'private', harnessType: 'codex' }
    expect(namesRequestedSelection('switch to Atlas for this', selection)).toBe(true)
    expect(namesRequestedSelection('switch to `atlas`', selection)).toBe(true)
    expect(namesRequestedSelection('keep thinking without another flag', selection)).toBe(false)
    expect(namesRequestedSelection('explain "switch to Atlas"', selection)).toBe(false)
    expect(namesRequestedSelection('review this\n> use atlas\n```use atlas```', selection)).toBe(false)
    expect(namesRequestedSelection('use atlaswork', selection)).toBe(false)
  })

  test('accepts the classifier vocabulary for default and versioned built-in selections', () => {
    const cases = [
      ['use claude', 'claude-opus-5-5'],
      ['using claude:', 'claude-opus-5-5'],
      ['switch to codex 5.5', 'gpt-5.5'],
      ['switch to codex5.5', 'gpt-5.5'],
      ['use gpt 5.5 pro', 'gpt-5.5-pro'],
      ['use codex 5.4 pro', 'gpt-5.4-pro'],
      ['use 5.6 sol', 'gpt-5.6-sol'],
      ['use opus 4.7', 'claude-opus-4-7']
    ] as const
    for (const [text, model] of cases) {
      expect(namesRequestedSelection(text, validateStrategyOverrides({ model }))).toBe(true)
    }
    for (const text of ['keep thinking', 'use codex 5.4', 'use codex 5.5.1',
      'explain "switch to codex 5.5"', '> switch to codex 5.5', '```switch to codex 5.5```']) {
      expect(namesRequestedSelection(text, validateStrategyOverrides({ model: 'gpt-5.5' }))).toBe(false)
    }
    expect(namesRequestedSelection('use claude', validateStrategyOverrides({ model: 'claude-opus-4-7' }))).toBe(false)
    expect(modelSelectionNames('gpt-5.5-pro')).toContain('codex 5.5 pro')
    expect(modelSelectionNames('gpt-5.6-sol')).not.toContain('sol')
  })

  test('exposes configured aliases to the classifier without hardcoded trigger words', async () => {
    let request: { questions: Array<{ name: string; choices?: Array<{ value: string }> }> } | undefined
    const strategy = createOpenAiMessageOverridesStrategy({ apiKey: 'test', model: 'classifier',
      fetch: (async (_url: RequestInfo | URL, init?: RequestInit) => {
        request = JSON.parse(String(init?.body))
        return Response.json({ answers: [
          { name: 'selection_request', type: 'predicate', probability: 0.99 },
          { name: 'model', type: 'choice', choice: 'atlas-full' }
        ] })
      }) as unknown as typeof fetch
    })
    expect((await strategy({ text: 'use Atlas for this' })).overrides).toMatchObject({
      model: 'atlas-full', provider: 'private', harnessType: 'codex'
    })
    expect(request?.questions.find(question => question.name === 'model')?.choices).toContainEqual(
      expect.objectContaining({ value: 'private/atlas-full' }))
  })

  test('keeps model discussion on the existing selection when the classifier declines', async () => {
    const strategy = createOpenAiMessageOverridesStrategy({ apiKey: 'test', model: 'classifier',
      fetch: (async () => Response.json({ answers: [
        { name: 'selection_request', type: 'predicate', probability: 0.01 },
        { name: 'model', type: 'choice', choice: 'atlas-full' }
      ] })) as unknown as typeof fetch
    })
    expect((await strategy({ text: 'what is Atlas?' })).overrides).toEqual({})
  })

  test('normalizes efforts and supplies the actual model default', () => {
    expect(reasoningForModel('codex', 'atlas-full', undefined, 'private')).toBe('max')
    expect(reasoningForModel('codex', 'atlas-full', 'xhigh', 'private')).toBe('max')
    expect(reasoningForModel('codex', 'atlas-full', 'low', 'private')).toBe('low')
    expect(reasoningForModel('codex', 'atlas-full', 'ultra', 'private')).toBe('max')
    const footer = buildSlackResponseContextBlock({ metadataEnabled: true, model: 'atlas-full',
      provider: 'private', harnessType: 'codex', reasoning: 'max' })
    expect(footer?.elements[0]?.text).toBe('Atlas Full · Private Inference · Codex · Max')
  })
})
