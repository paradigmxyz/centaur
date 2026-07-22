import { describe, expect, test } from 'bun:test'
import { isSlackStopCommand } from '../src/stop-command'

describe('Slack stop command detection', () => {
  test('matches mention plus stop keyword', () => {
    expect(isSlackStopCommand({ text: '<@UCENTAUR> stop' })).toBe(true)
    expect(isSlackStopCommand({ text: 'please <@UCENTAUR> STOP now' })).toBe(true)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> Stop' })).toBe(true)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> stoppp' })).toBe(true)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> could you stop the execution?' })).toBe(true)
  })

  test('matches kill, end, cancel, and common variants', () => {
    for (const text of [
      'kill',
      'kill it',
      'killed',
      'killing',
      'end',
      'end it',
      'ended',
      'ending',
      'cancel',
      'cancels',
      'canceled',
      'cancelled',
      'canceling',
      'cancelling'
    ]) {
      expect(isSlackStopCommand({ text: `<@UCENTAUR> ${text}` })).toBe(true)
    }
  })

  test('matches Chat-SDK-normalized mentions plus stop keyword', () => {
    // The Chat SDK rewrites <@U123|name> to @name and the bot's own <@U123>
    // to @U123 before handlers run, so live message.text carries these forms.
    expect(isSlackStopCommand({ text: '@centaur_ai stop' })).toBe(true)
    expect(isSlackStopCommand({ text: '@U08TEST123 stop' })).toBe(true)
    expect(isSlackStopCommand({ text: 'please @centaur_ai STOP now' })).toBe(true)
    expect(isSlackStopCommand({ text: '@centaur_ai could you stop the execution?' })).toBe(true)
    expect(isSlackStopCommand({ text: '@centaur_ai cancel' })).toBe(true)
  })

  test('does not match unrelated normalized mentions', () => {
    expect(isSlackStopCommand({ text: '@centaur_ai status' })).toBe(false)
    expect(isSlackStopCommand({ text: '@centaur_ai stopping by to ask' })).toBe(false)
    expect(isSlackStopCommand({ text: '@centaur_ai if so, stop.' })).toBe(false)
    expect(
      isSlackStopCommand({ text: '@centaur_ai please check the service; if it is broken, stop.' })
    ).toBe(false)
    expect(isSlackStopCommand({ text: 'cancel the invite for user@example.com' })).toBe(false)
  })

  test('does not match unrelated mentions', () => {
    expect(isSlackStopCommand({ text: '<@UCENTAUR> status' })).toBe(false)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> stopping by to ask' })).toBe(false)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> run an end-to-end test' })).toBe(false)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> cancellation policy' })).toBe(false)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> if so, stop.' })).toBe(false)
    expect(
      isSlackStopCommand({ text: '<@UCENTAUR> please check the service; if it is broken, stop.' })
    ).toBe(false)
  })
})
