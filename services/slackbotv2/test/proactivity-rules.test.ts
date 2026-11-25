import { afterEach, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { loadProactivityRules, resolveProactivityRules } from '../src/proactivity-rules'

describe('proactivity rules', () => {
  let dir = ''
  afterEach(() => {
    if (dir) rmSync(dir, { force: true, recursive: true })
  })

  function writeRules(files: Record<string, string>): string {
    dir = mkdtempSync(join(tmpdir(), 'proactivity-rules-'))
    // Mirror a mounted ConfigMap: files are symlinks into a hidden data dir.
    mkdirSync(join(dir, '..data'))
    for (const [name, content] of Object.entries(files)) {
      writeFileSync(join(dir, '..data', name), content)
      symlinkSync(join('..data', name), join(dir, name))
    }
    return dir
  }

  test('load ordered default rules that channel rule files replace or turn off', () => {
    writeRules({
      'default.json': JSON.stringify([{ name: 'services', when: 'Questions about our services.\n' }]),
      'C0ONCALL.json': JSON.stringify([
        { context: '\nPost the runbook first.\n', name: 'incident', when: 'An active incident.' },
        { name: 'rollback', when: 'A rollback request.', context: '  ' }
      ]),
      'C0SOCIAL.json': '[]'
    })
    writeFileSync(join(dir, 'README.txt'), 'not rules')

    const rules = loadProactivityRules(dir)

    expect(resolveProactivityRules(rules, 'slack:C0OTHER:1.000')).toEqual([
      { name: 'services', when: 'Questions about our services.' }
    ])
    expect(resolveProactivityRules(rules, 'slack:T0TEAM:C0ONCALL:1.000')).toEqual([
      { context: 'Post the runbook first.', name: 'incident', when: 'An active incident.' },
      { name: 'rollback', when: 'A rollback request.' }
    ])
    expect(resolveProactivityRules(rules, 'slack:C0SOCIAL:1.000')).toBeUndefined()
    expect(rules.channels).not.toHaveProperty('README')
  })

  test('reject a rule file that is not a list of named rules with criteria', () => {
    for (const content of [
      '{"name": "x", "when": "y"}',
      '[{"name": "x"}]',
      '[{"name": "x", "when": "y"}, {"name": "x", "when": "z"}]',
      '[{"name": " ", "when": "y"}]',
      JSON.stringify(Array.from({ length: 21 }, (_, index) => ({ name: `r${index}`, when: 'y' }))),
      'not json'
    ]) {
      writeRules({ 'C0BAD.json': content })
      expect(() => loadProactivityRules(dir)).toThrow()
      rmSync(dir, { force: true, recursive: true })
    }
  })

  test('throw to the caller when the rules directory cannot be read', () => {
    expect(() => loadProactivityRules(join(tmpdir(), 'missing-proactivity-rules'))).toThrow()
  })

  test('apply no rules when none are configured', () => {
    expect(resolveProactivityRules({}, 'slack:C0OTHER:1.000')).toBeUndefined()
  })
})
