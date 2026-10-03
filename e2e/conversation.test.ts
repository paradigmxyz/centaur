// A user starts a Slack thread with each harness and follows up in it. Each
// answer must reach the thread once, the harness must still hold the first
// exchange in its own session, the proxy must swap in the real provider key,
// and the thread must stay on one sandbox running the requested harness.
import { expect, test } from 'bun:test'
import { model, providerCredentials, slack, turnTimeoutMs } from './lib'

for (const harness of ['codex', 'claudecode']) {
  test.concurrent(`${harness} answers and remembers a Slack thread`, async () => {
    const thread = await slack.mention(`--${harness} Start the thread.`, model.says('First answer.'))
    const first = await thread.nextTurn()
    expect(first.reply).toBe('First answer.')

    await thread.mention('Follow up.', model.says('Second answer.'))
    const second = await thread.nextTurn()
    expect(second.reply).toBe('Second answer.')

    for (const turn of [first, second]) {
      expect(turn.execution.status).toBe('completed')
      expect(turn.request?.credential).toBe(providerCredentials[harness])
    }
    // slackbotv2 quotes earlier Slack messages into each turn, so only an
    // assistant turn in the request proves the harness kept its own session.
    expect(second.request?.assistantTurns).toContain('First answer.')
    expect((await thread.sandboxes()).map(sandbox => sandbox.harness)).toEqual([harness])
  }, 2 * turnTimeoutMs)
}
