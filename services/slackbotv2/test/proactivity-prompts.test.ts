import { afterEach, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { loadProactivityPrompts, resolveProactivityPrompt } from '../src/proactivity-prompts'

describe('proactivity prompts', () => {
  let dir = ''
  afterEach(() => {
    if (dir) rmSync(dir, { force: true, recursive: true })
  })

  test('load a default prompt that channel prompt files replace or turn off', () => {
    dir = mkdtempSync(join(tmpdir(), 'proactivity-prompts-'))
    // Mirror a mounted ConfigMap: files are symlinks into a hidden data dir.
    mkdirSync(join(dir, '..data'))
    const files = {
      'default.md': 'Questions about our services.\n',
      'C0ONCALL.md': '\nQuestions about an active incident:\n- outages\n- rollbacks\n',
      'C0SOCIAL.md': ''
    }
    for (const [name, content] of Object.entries(files)) {
      writeFileSync(join(dir, '..data', name), content)
      symlinkSync(join('..data', name), join(dir, name))
    }
    writeFileSync(join(dir, 'README.txt'), 'not a prompt')

    const prompts = loadProactivityPrompts(dir)

    expect(resolveProactivityPrompt(prompts, 'slack:C0OTHER:1.000')).toBe('Questions about our services.')
    expect(resolveProactivityPrompt(prompts, 'slack:T0TEAM:C0ONCALL:1.000'))
      .toBe('Questions about an active incident:\n- outages\n- rollbacks')
    expect(resolveProactivityPrompt(prompts, 'slack:C0SOCIAL:1.000')).toBeUndefined()
    expect(prompts.channels).not.toHaveProperty('README')
  })

  test('throw to the caller when the prompt directory cannot be read', () => {
    expect(() => loadProactivityPrompts(join(tmpdir(), 'missing-proactivity-prompts'))).toThrow()
  })

  test('apply no prompt when none is configured', () => {
    expect(resolveProactivityPrompt({}, 'slack:C0OTHER:1.000')).toBeUndefined()
  })
})
