import { describe, expect, it } from 'bun:test'
import type { Logger, Message } from 'chat'
import { isAllowedSlackMessage } from '../src/slack-events'
import type { SlackbotV2Options } from '../src/types'

const logger: Logger = {
  debug: () => {},
  info: () => {},
  warn: () => {},
  error: () => {},
  child: () => logger
}

function botMessage(botId: string): Message {
  return {
    author: { isBot: true },
    id: '1700000000.000001',
    raw: { bot_id: botId, subtype: 'bot_message' },
    threadId: 'C1:1700000000.000001'
  } as Message
}

function options(fetchImpl: SlackbotV2Options['fetch']): SlackbotV2Options {
  return {
    apiUrl: 'http://session.test/',
    botToken: 'xoxb-test',
    fetch: fetchImpl,
    signingSecret: 'test',
    slackApiUrl: 'http://slack.test/api/',
    triggerBotAllowlist: ['UALLOWED']
  }
}

describe('Slack trigger bot allowlist', () => {
  it('resolves a bot-only event to its allowlisted bot user and caches the mapping', async () => {
    let requests = 0
    const config = options(async input => {
      requests += 1
      expect(String(input)).toBe('http://slack.test/api/bots.info?bot=BCHANNELBOT')
      return Response.json({
        ok: true,
        bot: { id: 'BCHANNELBOT', app_id: 'AALERTS', user_id: 'UALLOWED' }
      })
    })

    expect(await isAllowedSlackMessage(botMessage('BCHANNELBOT'), config, logger)).toBe(true)
    expect(await isAllowedSlackMessage(botMessage('BCHANNELBOT'), config, logger)).toBe(true)
    expect(requests).toBe(1)
  })

  it('allows a webhook bot belonging to the same app as an allowlisted bot user', async () => {
    const requests = new Map<string, number>()
    const config = options(async input => {
      const url = String(input)
      requests.set(url, (requests.get(url) ?? 0) + 1)
      if (url.includes('/bots.info')) {
        return Response.json({
          ok: true,
          bot: { id: 'BWEBHOOK', app_id: 'AALERTS' }
        })
      }
      return Response.json({
        ok: true,
        user: {
          id: 'UALLOWED',
          profile: { api_app_id: 'AALERTS', bot_id: 'BAPPUSER' }
        }
      })
    })

    expect(await isAllowedSlackMessage(botMessage('BWEBHOOK'), config, logger)).toBe(true)
    expect(await isAllowedSlackMessage(botMessage('BWEBHOOK'), config, logger)).toBe(true)
    expect(requests).toEqual(new Map([
      ['http://slack.test/api/bots.info?bot=BWEBHOOK', 1],
      ['http://slack.test/api/users.info?user=UALLOWED', 1]
    ]))
  })

  it('stays fail-closed when a bot-only event cannot be mapped to an allowlisted identity', async () => {
    const config = options(async () =>
      Response.json({
        ok: true,
        bot: { id: 'BOTHER', app_id: 'AOTHER', user_id: 'UOTHER' }
      })
    )

    expect(await isAllowedSlackMessage(botMessage('BOTHER'), config, logger)).toBe(false)
  })

  it('allows an explicitly scoped bot identifier without an identity lookup', async () => {
    let requests = 0
    const config = {
      ...options(async () => {
        requests += 1
        return Response.json({ ok: true })
      }),
      triggerBotAllowlist: ['bot:BCHANNELBOT']
    }

    expect(await isAllowedSlackMessage(botMessage('BCHANNELBOT'), config, logger)).toBe(true)
    expect(requests).toBe(0)
  })

  it('does not treat bot or app identifiers as public allowlist entries', async () => {
    let requests = 0
    const config = {
      ...options(async () => {
        requests += 1
        return Response.json({ ok: true })
      }),
      triggerBotAllowlist: ['BCHANNELBOT', 'AALERTS']
    }

    expect(await isAllowedSlackMessage(botMessage('BCHANNELBOT'), config, logger)).toBe(false)
    expect(requests).toBe(0)
  })
})
