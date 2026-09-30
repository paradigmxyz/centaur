import { expect, test } from 'bun:test'
import { createHmac } from 'node:crypto'
import { createMemoryState } from '@chat-adapter/state-memory'
import { crewFetch, crewOptions, crewSessionKey, parseCrew } from '../src/crew'
import { createSlackbotV2, messageOverridesForText } from '../src/index'

const member = { id: 'atlas', appId: 'A1', teamId: 'T1', botUserId: 'U1', botToken: 'xoxb-test', signingSecret: 'atlas-secret' }
const base = { apiUrl: 'http://api.test', botToken: 'xoxb-base', signingSecret: 'base-secret', slackHomeTeamId: 'T1' }

test('validates config without reflecting secrets in errors', () => {
  expect(parseCrew(JSON.stringify([member]))).toEqual([member])
  expect(() => parseCrew(JSON.stringify([member, member]))).toThrow('Duplicate')
  expect(() => parseCrew(JSON.stringify([{ ...member, botToken: 'secret-value' }]))).toThrow('Invalid Crew botToken')
})

test('isolates sessions and state, preserves legacy keys, and pins instructions', async () => {
  const options = crewOptions(base, member)
  const other = crewOptions(base, { ...member, id: 'scout', appId: 'A2' })
  expect(options.stateKeyPrefix).not.toBe(other.stateKeyPrefix)
  expect(crewSessionKey(base, 'slack:T1:C1:123.456')).toBe('slack:T1:C1:123.456')
  expect(crewSessionKey(options, 'slack:T1:C1:123.456')).toBe('slack:T1:A1:C1:123.456')
  expect(crewSessionKey(other, 'slack:D1:123.456')).toBe('slack:T1:A2:D1:123.456')
  expect(() => crewSessionKey(options, 'slack:T2:C1:123.456')).toThrow('workspace mismatch')
  const override = await messageOverridesForText(options, '--persona=eng hello', {
    threadId: 'slack:C1:123.456', messageId: '123.456', includeContext: false,
    mode: 'execute', openStream: false, startedAtMs: 0
  })
  expect(override.overrides.personaId).toBe('atlas')
})

test('routes a signed Slack challenge through the selected verifier', async () => {
  const defaultApp = createSlackbotV2({ ...base, state: createMemoryState(), recoverRenderObligationsOnStart: false }).app
  const atlas = createSlackbotV2({ ...crewOptions(base, member), state: createMemoryState(), recoverRenderObligationsOnStart: false }).app
  const fetch = crewFetch(defaultApp, new Map([['atlas', atlas]]))
  const body = JSON.stringify({ type: 'url_verification', challenge: 'crew-challenge' })
  const ts = String(Math.floor(Date.now() / 1000))
  const request = (key: string, path = 'atlas') => new Request(`http://localhost/api/webhooks/slack/crew/${path}`, {
    method: 'POST', body, headers: {
      'content-type': 'application/json', 'x-slack-request-timestamp': ts,
      'x-slack-signature': `v0=${createHmac('sha256', key).update(`v0:${ts}:${body}`).digest('hex')}`
    }
  })
  const accepted = await fetch(request(member.signingSecret))
  expect(accepted.status).toBe(200)
  expect(await accepted.text()).toContain('crew-challenge')
  expect((await fetch(request(base.signingSecret))).status).toBe(401)
  expect((await fetch(request(member.signingSecret, 'missing'))).status).toBe(404)
})
