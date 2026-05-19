import { describe, expect, it } from 'bun:test'
import { duplicateSlackAlertText } from './duplicate-alert'

describe('duplicateSlackAlertText', () => {
  it('includes Slack and Codex thread identifiers without message text', () => {
    expect(
      duplicateSlackAlertText({
        dedupe_key: 'message:T123:C123:1778883099.579530',
        event_id: 'Ev123',
        team_id: 'T123',
        channel_id: 'C123',
        thread_ts: '1778883099.579529',
        message_ts: '1778883099.579530',
        event_type: 'message',
        codex_thread_id: 'T-019e28c1-08bb-777d-9a2e-74a393296b28'
      })
    ).toBe(
      [
        '*Slack duplicate message skipped*',
        '*Channel:* `C123`',
        '*Thread:* `1778883099.579529`',
        '*Message:* `1778883099.579530`',
        '*Event type:* `message`',
        '*Event ID:* `Ev123`',
        '*Codex thread:* `T-019e28c1-08bb-777d-9a2e-74a393296b28`',
        '*Dedupe key:* `message:T123:C123:1778883099.579530`'
      ].join('\n')
    )
  })
})
