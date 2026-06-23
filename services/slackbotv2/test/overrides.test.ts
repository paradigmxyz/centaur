import { describe, expect, test } from 'bun:test'
import { extractMessageOverrides } from '../src/overrides'

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

  test('--model expands claude aliases to full model ids', () => {
    expect(extractMessageOverrides('--claude --model opus go')).toEqual({
      cleanedText: 'go',
      harnessType: 'claudecode',
      model: 'claude-opus-4-8'
    })
    expect(extractMessageOverrides('--model Sonnet go').model).toBe('claude-sonnet-4-6')
    expect(extractMessageOverrides('--model fable go').model).toBe('claude-fable-5')
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
})
