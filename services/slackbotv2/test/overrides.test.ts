import { describe, expect, test } from 'bun:test'
import { extractMessageOverrides } from '../src/overrides'

describe('extractMessageOverrides', () => {
  test('returns text untouched without flags', () => {
    const result = extractMessageOverrides('review this PR --not-a-known-flag stays')
    expect(result).toEqual({
      cleanedText: 'review this PR --not-a-known-flag stays',
      harnessType: undefined,
      model: undefined
    })
  })

  test('parses harness flags', () => {
    expect(extractMessageOverrides('--claude review this')).toEqual({
      cleanedText: 'review this',
      harnessType: 'claudecode',
      model: undefined
    })
    expect(extractMessageOverrides('--claude-code review this').harnessType).toBe('claudecode')
    expect(extractMessageOverrides('--amp review this').harnessType).toBe('amp')
    expect(extractMessageOverrides('--codex review this').harnessType).toBe('codex')
  })

  test('parses harness flag anywhere in the message', () => {
    expect(extractMessageOverrides('review this --amp please')).toEqual({
      cleanedText: 'review this please',
      harnessType: 'amp',
      model: undefined
    })
  })

  test('is case-insensitive', () => {
    expect(extractMessageOverrides('--Claude review').harnessType).toBe('claudecode')
  })

  test('parses --model with space or equals', () => {
    expect(extractMessageOverrides('--claude --model claude-sonnet-4-6 fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'claudecode',
      model: 'claude-sonnet-4-6'
    })
    expect(extractMessageOverrides('--codex --model=gpt-5.2 fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: 'gpt-5.2'
    })
  })

  test('model shortcuts set model and imply claude-code', () => {
    expect(extractMessageOverrides('--opus fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'claudecode',
      model: 'claude-opus-4-8'
    })
    expect(extractMessageOverrides('--sonnet fix it').model).toBe('claude-sonnet-4-6')
    expect(extractMessageOverrides('--haiku fix it').model).toBe('claude-haiku-4-5')
  })

  test('explicit flags win over shortcut implications', () => {
    expect(extractMessageOverrides('--codex --opus fix it')).toEqual({
      cleanedText: 'fix it',
      harnessType: 'codex',
      model: 'claude-opus-4-8'
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
      model: undefined
    })
  })

  test('--model without a value is left untouched', () => {
    expect(extractMessageOverrides('what does --model do?')).toEqual({
      cleanedText: 'what does --model do?',
      harnessType: undefined,
      model: undefined
    })
  })
})
