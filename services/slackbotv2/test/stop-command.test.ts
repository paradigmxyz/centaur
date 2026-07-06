import { describe, expect, test } from 'bun:test'
import { isSlackStopCommand } from '../src/stop-command'

describe('Slack stop command detection', () => {
  test('matches mention plus stop keyword', () => {
    expect(isSlackStopCommand({ text: '<@UCENTAUR> stop' })).toBe(true)
    expect(isSlackStopCommand({ text: 'please <@UCENTAUR> STOP now' })).toBe(true)
  })

  test('does not match unrelated mentions', () => {
    expect(isSlackStopCommand({ text: '<@UCENTAUR> status' })).toBe(false)
    expect(isSlackStopCommand({ text: '<@UCENTAUR> stopping by to ask' })).toBe(false)
  })
})
