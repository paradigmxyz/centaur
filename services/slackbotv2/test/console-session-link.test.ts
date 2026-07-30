import { describe, expect, test } from 'bun:test'
import {
  buildConsoleSessionContextBlock,
  consoleSessionUrl,
  defaultModelForHarness,
  defaultReasoningForHarness,
  effectiveReasoningForHarness,
  harnessDisplayName,
  reasoningForModel
} from '../src/console-session-link'
import claudeSettings from '../../../harness/claude/settings.json'
import codexConfig from '../../../harness/codex/config.toml'

describe('harnessDisplayName', () => {
  test('maps known harness wire values to display names', () => {
    expect(harnessDisplayName('codex')).toBe('Codex')
    expect(harnessDisplayName('nanocodex')).toBe('Nanocodex')
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

describe('reasoningForModel', () => {
  const allEfforts = ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']
  const standardEfforts = ['none', 'low', 'medium', 'high', 'xhigh']
  const proEfforts = ['medium', 'high', 'xhigh']
  const codexModelEfforts = ['low', 'medium', 'high', 'xhigh']
  const effortsByModel: Record<string, string[]> = {
    'gpt-5.2': standardEfforts,
    'gpt-5.2-codex': codexModelEfforts,
    'gpt-5.4': standardEfforts,
    'gpt-5.4-mini': standardEfforts,
    'gpt-5.4-nano': standardEfforts,
    'gpt-5.4-pro': proEfforts,
    'gpt-5.5': standardEfforts,
    'gpt-5.5-pro': proEfforts,
    'gpt-5.6-luna': [...standardEfforts, 'max'],
    'gpt-5.6-sol': [...standardEfforts, 'max'],
    'gpt-5.6-terra': [...standardEfforts, 'max']
  }

  test('matches the reasoning efforts advertised by supported Codex models', () => {
    for (const [model, supportedEfforts] of Object.entries(effortsByModel)) {
      for (const effort of allEfforts) {
        expect(reasoningForModel('codex', model, effort)).toBe(
          supportedEfforts.includes(effort) ? effort : undefined
        )
      }
    }
  })

  test('validates Nanocodex against its selected model after mapping minimal to low', () => {
    for (const [model, supportedEfforts] of Object.entries(effortsByModel)) {
      for (const effort of allEfforts) {
        const effectiveEffort = effort === 'minimal' ? 'low' : effort
        expect(reasoningForModel('nanocodex', model, effort)).toBe(
          supportedEfforts.includes(effectiveEffort) ? effort : undefined
        )
      }
    }
  })

  test('supports current model aliases and snapshots without widening their effort sets', () => {
    expect(reasoningForModel('codex', 'gpt-5.6', 'max')).toBe('max')
    expect(reasoningForModel('nanocodex', 'gpt-5.6', 'max')).toBe('max')
    expect(reasoningForModel('codex', 'gpt-5.6-sol-2026-07-01', 'minimal')).toBeUndefined()
    expect(reasoningForModel('nanocodex', 'gpt-5.6-sol-2026-07-01', 'minimal')).toBe(
      'minimal'
    )
    expect(reasoningForModel('codex', 'gpt-5.5-pro-2026-07-01', 'low')).toBeUndefined()
    expect(reasoningForModel('nanocodex', 'gpt-5.5-pro-2026-07-01', 'low')).toBeUndefined()
    expect(reasoningForModel('codex', 'gpt-5.4-2026-03-05', 'xhigh')).toBe('xhigh')
    expect(reasoningForModel('codex', 'gpt-5.3', 'high')).toBeUndefined()
  })

  test('rejects Codex efforts for the currently selected non-Codex model', () => {
    expect(reasoningForModel('claudecode', 'claude-opus-4-8', 'high')).toBeUndefined()
    expect(reasoningForModel('amp', 'fast', 'low')).toBeUndefined()
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
    expect(defaultModelForHarness('nanocodex')).toBe(bakedCodexModel)
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

describe('defaultReasoningForHarness', () => {
  const bakedCodexReasoning = (codexConfig as { model_reasoning_effort: string })
    .model_reasoning_effort

  test('shares the baked Codex reasoning default with Nanocodex', () => {
    expect(bakedCodexReasoning).toBe('low')
    expect(defaultReasoningForHarness('codex')).toBe(bakedCodexReasoning)
    expect(defaultReasoningForHarness('nanocodex')).toBe(bakedCodexReasoning)
    expect(defaultReasoningForHarness('claudecode')).toBeUndefined()
  })

  test('prefers a deployment-configured Codex-compatible default', () => {
    const configured = { codex: 'HIGH', nanocodex: 'HIGH' }
    expect(defaultReasoningForHarness('codex', configured)).toBe('high')
    expect(defaultReasoningForHarness('nanocodex', configured)).toBe('high')
  })

  test('reports the effort the selected harness actually runs', () => {
    expect(effectiveReasoningForHarness('codex', 'xhigh')).toBe('xhigh')
    expect(effectiveReasoningForHarness('nanocodex', 'minimal')).toBe('low')
    expect(effectiveReasoningForHarness('claudecode', 'high')).toBeUndefined()
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
      model: 'gpt-5.2',
      reasoning: 'xhigh'
    })
    expect(block).toEqual({
      type: 'context',
      elements: [
        {
          type: 'mrkdwn',
          text:
            '<https://console.centaur.dev/console/threads?thread=slack%3AC123%3A1700000000.000100|Open chat in Console> · GPT-5.2 · Codex · XHigh'
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

  test('shows the resolved Nanocodex harness', () => {
    const block = buildConsoleSessionContextBlock({
      consoleBaseUrl: 'https://console.centaur.dev',
      threadKey: 'slack:C1:1',
      harnessType: 'nanocodex',
      model: 'gpt-5.6-sol',
      reasoning: 'low'
    })

    expect(block?.elements[0]?.text).toBe(
      '<https://console.centaur.dev/console/threads?thread=slack%3AC1%3A1|Open chat in Console> · GPT-5.6-SOL · Nanocodex · Low'
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
