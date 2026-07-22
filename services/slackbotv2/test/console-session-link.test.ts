import { describe, expect, test } from 'bun:test'
import {
  buildConsoleSessionContextBlock,
  consoleSessionUrl,
  defaultModelForHarness,
  harnessDisplayName
} from '../src/console-session-link'
import claudeSettings from '../../../harness/claude/settings.json'
import codexConfig from '../../../harness/codex/config.toml'

describe('harnessDisplayName', () => {
  test('maps known harness wire values to display names', () => {
    expect(harnessDisplayName('codex')).toBe('Codex')
    expect(harnessDisplayName('claudecode')).toBe('Claude Code')
    expect(harnessDisplayName('amp')).toBe('Amp')
  })

  test('is case-insensitive and trims', () => {
    expect(harnessDisplayName(' Codex ')).toBe('Codex')
    expect(harnessDisplayName('CLAUDECODE')).toBe('Claude Code')
  })

  test('title-cases unknown harnesses', () => {
    expect(harnessDisplayName('my-custom-harness')).toBe('My Custom Harness')
    expect(harnessDisplayName('gemini')).toBe('Gemini')
  })

  test('returns undefined for empty or missing values', () => {
    expect(harnessDisplayName(undefined)).toBeUndefined()
    expect(harnessDisplayName(null)).toBeUndefined()
    expect(harnessDisplayName('')).toBeUndefined()
    expect(harnessDisplayName('   ')).toBeUndefined()
  })
})

describe('defaultModelForHarness', () => {
  const bakedClaudeModel = claudeSettings.model
  const bakedCodexModel = (codexConfig as { model: string }).model

  test('reads the baked default model from the repo harness config files', () => {
    expect(bakedClaudeModel).toBeTruthy()
    expect(bakedCodexModel).toBeTruthy()
    expect(defaultModelForHarness('claudecode')).toBe(bakedClaudeModel)
    expect(defaultModelForHarness('codex')).toBe(bakedCodexModel)
  })

  test('prefers the deployment-configured model over the baked default', () => {
    const configured = { claudecode: 'claude-fable-5' }
    expect(defaultModelForHarness('claudecode', configured)).toBe('claude-fable-5')
    expect(defaultModelForHarness('codex', configured)).toBe(bakedCodexModel)
    expect(defaultModelForHarness('claudecode', { claudecode: '   ' })).toBe(bakedClaudeModel)
  })

  test('is case-insensitive and trims', () => {
    expect(defaultModelForHarness(' CLAUDECODE ')).toBe(bakedClaudeModel)
  })

  test('returns undefined for harnesses without a fixed default', () => {
    expect(defaultModelForHarness('amp')).toBeUndefined()
    expect(defaultModelForHarness('gemini')).toBeUndefined()
    expect(defaultModelForHarness(undefined)).toBeUndefined()
    expect(defaultModelForHarness(null)).toBeUndefined()
    expect(defaultModelForHarness('')).toBeUndefined()
  })
})

describe('consoleSessionUrl', () => {
  test('builds the /console/threads URL with an encoded thread key', () => {
    expect(consoleSessionUrl('https://console.centaur.dev', 'slack:C123:1700000000.000100')).toBe(
      'https://console.centaur.dev/console/threads?thread=slack%3AC123%3A1700000000.000100'
    )
  })

  test('strips trailing slashes from the base URL', () => {
    expect(consoleSessionUrl('https://console.centaur.dev/', 'slack:C1:1')).toBe(
      'https://console.centaur.dev/console/threads?thread=slack%3AC1%3A1'
    )
  })

  test('returns undefined when no base URL is configured', () => {
    expect(consoleSessionUrl(undefined, 'slack:C1:1')).toBeUndefined()
    expect(consoleSessionUrl(null, 'slack:C1:1')).toBeUndefined()
    expect(consoleSessionUrl('   ', 'slack:C1:1')).toBeUndefined()
  })
})

describe('buildConsoleSessionContextBlock', () => {
  test('builds a context block with uppercased model then harness, middot separated', () => {
    const block = buildConsoleSessionContextBlock({
      consoleBaseUrl: 'https://console.centaur.dev',
      threadKey: 'slack:C123:1700000000.000100',
      harnessType: 'codex',
      model: 'gpt-5.2'
    })
    expect(block).toEqual({
      type: 'context',
      elements: [
        {
          type: 'mrkdwn',
          text:
            '<https://console.centaur.dev/console/threads?thread=slack%3AC123%3A1700000000.000100|Open chat in Console> · GPT-5.2 · Codex'
        }
      ]
    })
  })

  test('omits the model segment when no model is provided', () => {
    const block = buildConsoleSessionContextBlock({
      consoleBaseUrl: 'https://console.centaur.dev',
      threadKey: 'slack:C1:1',
      harnessType: 'claudecode'
    })
    expect(block?.elements[0]?.text).toBe(
      '<https://console.centaur.dev/console/threads?thread=slack%3AC1%3A1|Open chat in Console> · Claude Code'
    )
  })

  test('skips the block entirely when no console base URL is set', () => {
    expect(
      buildConsoleSessionContextBlock({
        consoleBaseUrl: undefined,
        threadKey: 'slack:C1:1',
        harnessType: 'codex',
        model: 'gpt-5.2'
      })
    ).toBeUndefined()
  })
})
