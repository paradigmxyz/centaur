import { describe, expect, test } from 'bun:test'
import {
  buildSlackResponseContextBlock,
  defaultModelForHarness,
  defaultReasoningForHarness,
  defaultServiceTierForHarness,
  effectiveReasoningForHarness,
  harnessDisplayName,
  modelDisplayName,
  personaFallbackNotice,
  reasoningForModel
} from '../src/response-context'
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
  const allEfforts = ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']
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
    'gpt-5.6-terra': [...standardEfforts, 'max'],
    'gpt-6-astra': ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'],
    'gpt-6-sol': [...standardEfforts, 'max'],
    'gpt-6.1-sol': ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'],
    'gpt-6-luna': [...standardEfforts, 'max']
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

  test('forwards Claude Code effort levels and rejects Codex-only efforts', () => {
    for (const effort of ['low', 'medium', 'high', 'xhigh', 'max']) {
      expect(reasoningForModel('claudecode', 'claude-opus-5-5', effort)).toBe(effort)
    }
    expect(reasoningForModel('claudecode', undefined, 'HIGH')).toBe('high')
    for (const effort of ['none', 'minimal', 'ultra']) {
      expect(reasoningForModel('claudecode', 'claude-opus-5-5', effort)).toBeUndefined()
    }
  })

  test('rejects Claude efforts the selected model does not support', () => {
    expect(reasoningForModel('claudecode', 'claude-haiku-4-5', 'max')).toBeUndefined()
    expect(reasoningForModel('claudecode', 'claude-haiku-4-5-20251001', 'low')).toBeUndefined()
    expect(reasoningForModel('claudecode', 'claude-sonnet-4-6', 'xhigh')).toBeUndefined()
    expect(reasoningForModel('claudecode', 'claude-sonnet-4-6', 'max')).toBe('max')
    expect(reasoningForModel('claudecode', 'claude-opus-4-5', 'max')).toBeUndefined()
    expect(reasoningForModel('claudecode', 'claude-opus-4-5', 'high')).toBe('high')
  })

  test('forwards Pi thinking levels for any model and rejects Codex-only efforts', () => {
    for (const effort of ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']) {
      expect(reasoningForModel('pi', undefined, effort)).toBe(effort)
    }
    expect(reasoningForModel('pi', 'openai/gpt-5.5', 'ultra')).toBeUndefined()
  })

  test('rejects efforts for harnesses without an effort control', () => {
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
    expect(bakedCodexReasoning).toBe('medium')
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
    expect(effectiveReasoningForHarness('claudecode', 'high')).toBe('high')
    expect(effectiveReasoningForHarness('claudecode', undefined)).toBeUndefined()
    expect(effectiveReasoningForHarness('amp', 'high')).toBeUndefined()
  })
})

describe('defaultServiceTierForHarness', () => {
  const bakedServiceTier = (codexConfig as { service_tier: string }).service_tier

  test('reports the baked Codex service tier only for the Codex harness', () => {
    expect(bakedServiceTier).toBe('fast')
    expect(defaultServiceTierForHarness('codex')).toBe(bakedServiceTier)
    expect(defaultServiceTierForHarness('nanocodex')).toBeUndefined()
    expect(defaultServiceTierForHarness('claudecode')).toBeUndefined()
  })
})

describe('modelDisplayName', () => {
  test('formats Claude and GPT IDs as product names and uppercases other models', () => {
    expect(modelDisplayName('claude-opus-5-5')).toBe('Opus 5.5')
    expect(modelDisplayName('claude-fable-5')).toBe('Fable 5')
    expect(modelDisplayName('claude-haiku-4-5-20251001')).toBe('Haiku 4.5')
    expect(modelDisplayName('claude-opus-5-fast')).toBe('Opus 5 Fast')
    expect(modelDisplayName('gpt-5.6-sol')).toBe('Sol 5.6')
    expect(modelDisplayName('gpt-6-astra')).toBe('Astra 6')
    expect(modelDisplayName('gpt-5.2')).toBe('GPT 5.2')
    expect(modelDisplayName('gpt-5.4-pro')).toBe('GPT 5.4 Pro')
    expect(modelDisplayName('gpt-5.2-codex')).toBe('GPT 5.2 Codex')
    expect(modelDisplayName('gpt-5.6')).toBe('GPT 5.6')
    expect(modelDisplayName('gpt-5.2-2025-12-11')).toBe('GPT 5.2')
    expect(modelDisplayName('gpt-5.6-sol-2026-07-01')).toBe('Sol 5.6')
  })

  test('ignores input casing', () => {
    expect(modelDisplayName('CLAUDE-OPUS-5-5')).toBe('Opus 5.5')
    expect(modelDisplayName('GPT-5.6-SOL')).toBe('Sol 5.6')
    expect(modelDisplayName('GPT-5.4-Pro')).toBe('GPT 5.4 Pro')
  })

  test('uppercases unrecognized model IDs', () => {
    expect(modelDisplayName('o3')).toBe('O3')
    expect(modelDisplayName('claude-opus-4-6[1m]')).toBe('CLAUDE-OPUS-4-6[1M]')
    expect(modelDisplayName('anthropic/claude-sonnet-4.5')).toBe('ANTHROPIC/CLAUDE-SONNET-4.5')
    expect(modelDisplayName('us.anthropic.claude-sonnet-4-5-20250929-v1:0')).toBe(
      'US.ANTHROPIC.CLAUDE-SONNET-4-5-20250929-V1:0'
    )
  })
})

describe('buildSlackResponseContextBlock', () => {
  test('builds a context block with model then harness, middot separated', () => {
    const block = buildSlackResponseContextBlock({
      harnessType: 'codex',
      metadataEnabled: true,
      model: 'gpt-5.2',
      reasoning: 'xhigh'
    })
    expect(block?.elements[0]?.text).toBe('GPT 5.2 · Codex · XHigh')
  })

  test('shows Claude models by product name', () => {
    const block = buildSlackResponseContextBlock({
      harnessType: 'claudecode',
      metadataEnabled: true,
      model: 'claude-opus-5-5'
    })
    expect(block?.elements[0]?.text).toBe('Opus 5.5 · Claude Code')
  })

  test('omits the model segment when no model is provided', () => {
    const block = buildSlackResponseContextBlock({
      harnessType: 'claudecode',
      metadataEnabled: true
    })
    expect(block?.elements[0]?.text).toBe('Claude Code')
  })

  test('shows the resolved Nanocodex harness', () => {
    const block = buildSlackResponseContextBlock({
      harnessType: 'nanocodex',
      metadataEnabled: true,
      model: 'gpt-5.6-sol',
      reasoning: 'low'
    })

    expect(block?.elements[0]?.text).toBe('Sol 5.6 · Nanocodex · Low')
  })

  test('skips the block when metadata and notices are absent', () => {
    expect(
      buildSlackResponseContextBlock({
        harnessType: 'codex',
        model: 'gpt-5.2'
      })
    ).toBeUndefined()
  })

  test('builds response metadata when enabled', () => {
    const block = buildSlackResponseContextBlock({
      harnessType: 'codex',
      metadataEnabled: true,
      model: 'gpt-5.6-sol',
      reasoning: 'low',
      serviceTier: 'fast'
    })

    expect(block?.elements[0]?.text).toBe('Sol 5.6 · Codex · Low · Fast')
  })

  test('renders and escapes a notice when response metadata is absent', () => {
    expect(
      buildSlackResponseContextBlock({
        notice: 'Persona "<unsafe&persona>" cannot be used.'
      })
    ).toEqual({
      type: 'context',
      elements: [
        {
          type: 'mrkdwn',
          text: ':warning: Persona "&lt;unsafe&amp;persona&gt;" cannot be used.'
        }
      ]
    })
  })
})

test('personaFallbackNotice describes the resolved fallback', () => {
  expect(personaFallbackNotice('honk', 'eng')).toBe(
    `Persona "honk" isn't available. Using "eng" instead.`
  )
  expect(personaFallbackNotice('honk', null)).toBe(
    `Persona "honk" isn't available. Continuing without a persona.`
  )
  expect(personaFallbackNotice(undefined, 'eng')).toBeUndefined()
})
