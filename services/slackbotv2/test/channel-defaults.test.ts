import { describe, expect, test } from 'bun:test'
import {
  channelIdFromThreadId,
  parseChannelDefaults,
  resolveChannelDefault
} from '../src/channel-defaults'

describe('parseChannelDefaults', () => {
  test('returns an empty map for unset or blank input', () => {
    expect(parseChannelDefaults(undefined)).toEqual({})
    expect(parseChannelDefaults('')).toEqual({})
    expect(parseChannelDefaults('   ')).toEqual({})
  })

  test('normalizes each channel object through the shared flag vocabulary', () => {
    const parsed = parseChannelDefaults(
      JSON.stringify({
        C0ENG: { harness: 'claude', model: 'opus', reasoning: 'high' },
        C0TRIAGE: { harness: 'codex', reasoning: 'low' },
        C0BEDROCK: { provider: 'bedrock', model: 'gpt-5.2' }
      })
    )
    expect(parsed).toEqual({
      // `claude` -> wire harness, `opus` -> full model id.
      C0ENG: { harnessType: 'claudecode', model: 'claude-opus-4-8', reasoning: 'high' },
      C0TRIAGE: { harnessType: 'codex', reasoning: 'low' },
      // A provider shortcut implies its harness, mirroring `--bedrock`.
      C0BEDROCK: { harnessType: 'codex', model: 'gpt-5.2', provider: 'amazon-bedrock' }
    })
  })

  test('allows reasoning alone, with no harness or model', () => {
    expect(parseChannelDefaults(JSON.stringify({ C0TRIAGE: { reasoning: 'low' } }))).toEqual({
      C0TRIAGE: { reasoning: 'low' }
    })
  })

  test('expands a model alias but leaves the harness to the explicit field', () => {
    // Like `--model opus` (not `--opus`): fields are independent, so a model
    // with no `harness` inherits the thread/deployment harness rather than one
    // guessed from the model name.
    expect(
      parseChannelDefaults(
        JSON.stringify({
          C0A: { model: 'opus' },
          C0B: { model: 'gpt-5.2' }
        })
      )
    ).toEqual({
      C0A: { model: 'claude-opus-4-8' },
      C0B: { model: 'gpt-5.2' }
    })
  })

  test('reports unknown field values and skips an entry that resolves to nothing', () => {
    const reasons: string[] = []
    const parsed = parseChannelDefaults(
      JSON.stringify({
        C0BAD: { harness: 'gpt', reasoning: 'turbo' },
        C0OK: { harness: 'codex' }
      }),
      reason => reasons.push(reason)
    )
    expect(parsed).toEqual({ C0OK: { harnessType: 'codex' } })
    expect(reasons.some(r => r.includes('C0BAD') && r.includes('unknown harness'))).toBe(true)
    expect(reasons.some(r => r.includes('C0BAD') && r.includes('unknown reasoning'))).toBe(true)
    expect(reasons.some(r => r.includes('C0BAD') && r.includes('no usable'))).toBe(true)
  })

  test('reports and skips a non-object entry', () => {
    const reasons: string[] = []
    const parsed = parseChannelDefaults(
      JSON.stringify({ C0ENG: '--claude', C0OK: { harness: 'codex' } }),
      reason => reasons.push(reason)
    )
    expect(parsed).toEqual({ C0OK: { harnessType: 'codex' } })
    expect(reasons.some(r => r.includes('C0ENG') && r.includes('expected an object'))).toBe(true)
  })

  test('reports and ignores invalid JSON without throwing', () => {
    const reasons: string[] = []
    expect(parseChannelDefaults('{not json', reason => reasons.push(reason))).toEqual({})
    expect(reasons).toHaveLength(1)
    expect(reasons[0]).toContain('invalid JSON')
  })

  test('reports and ignores a non-object top level', () => {
    const reasons: string[] = []
    expect(parseChannelDefaults('["C0ENG"]', reason => reasons.push(reason))).toEqual({})
    expect(reasons[0]).toContain('object')
  })
})

describe('channelIdFromThreadId', () => {
  test('extracts the channel segment from a slack thread key', () => {
    expect(channelIdFromThreadId('slack:C0ENG:1700000000.0001')).toBe('C0ENG')
    expect(channelIdFromThreadId('slack:T0TEAM:C0ENG:1700000000.0001')).toBe('C0ENG')
    expect(channelIdFromThreadId('slack:D0DM')).toBe('D0DM')
    expect(channelIdFromThreadId('slack:G0GROUP:ts')).toBe('G0GROUP')
  })

  test('returns undefined when no conversation segment is present', () => {
    expect(channelIdFromThreadId('web:t1')).toBeUndefined()
    expect(channelIdFromThreadId('slack')).toBeUndefined()
  })
})

describe('resolveChannelDefault', () => {
  const defaults = { C0ENG: { harnessType: 'claudecode', model: 'claude-opus-4-8' } }

  test('returns the default for a matching channel', () => {
    expect(resolveChannelDefault(defaults, 'slack:C0ENG:1700000000.0001')).toEqual({
      harnessType: 'claudecode',
      model: 'claude-opus-4-8'
    })
  })

  test('returns undefined for an unmapped channel or missing config', () => {
    expect(resolveChannelDefault(defaults, 'slack:C0OTHER:ts')).toBeUndefined()
    expect(resolveChannelDefault(undefined, 'slack:C0ENG:ts')).toBeUndefined()
    expect(resolveChannelDefault(defaults, 'web:t1')).toBeUndefined()
  })
})
