import { describe, expect, it } from 'bun:test'
import { loadConfig } from './config'

const baseEnv: Record<string, string> = {
  NODE_ENV: 'test',
  CENTAUR_API_URL: 'http://centaur-api.test'
}

describe('loadConfig', () => {
  it('defaults SLACKBOT_THREAD_FOLLOW off', () => {
    expect(loadConfig(env()).SLACKBOT_THREAD_FOLLOW).toBe(false)
  })

  it('parses explicit true and false SLACKBOT_THREAD_FOLLOW values', () => {
    for (const value of ['1', 'true', 'TRUE', 'yes', 'on']) {
      expect(loadConfig(env({ SLACKBOT_THREAD_FOLLOW: value })).SLACKBOT_THREAD_FOLLOW).toBe(true)
    }
    for (const value of ['0', 'false', 'FALSE', 'no', 'off', '']) {
      expect(loadConfig(env({ SLACKBOT_THREAD_FOLLOW: value })).SLACKBOT_THREAD_FOLLOW).toBe(false)
    }
  })

  it('rejects ambiguous SLACKBOT_THREAD_FOLLOW values', () => {
    expect(() => loadConfig(env({ SLACKBOT_THREAD_FOLLOW: 'enabled' }))).toThrow(
      /Invalid boolean value/
    )
  })
})

function env(overrides: Record<string, string> = {}): NodeJS.ProcessEnv {
  return { ...baseEnv, ...overrides } as NodeJS.ProcessEnv
}
