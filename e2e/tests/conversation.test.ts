// A user starts a Slack thread with each harness and follows up in it. Each
// answer must reach the thread once, the harness must still hold the first
// exchange in its own session, and the thread must stay on one sandbox running
// the requested harness.
import { expect, test } from 'bun:test'
import { model, slack, turnTimeoutMs } from '../lib'

for (const harness of ['codex', 'claudecode']) {
  test.concurrent(`${harness} answers and remembers a Slack thread`, async () => {
    const firstAnswer = model.says('First answer.')
    const secondAnswer = model.says('Second answer.')

    const thread = await slack.mention(`--${harness} Start the thread.`, firstAnswer)
    const first = await thread.nextTurn()
    expect(first.reply).toBe(firstAnswer.text)

    await thread.mention('Follow up.', secondAnswer)
    const second = await thread.nextTurn()
    expect(second.reply).toBe(secondAnswer.text)

    for (const turn of [first, second]) expect(turn.execution.status).toBe('completed')
    // slackbotv2 quotes earlier Slack messages into each turn, so only an
    // assistant turn in the request proves the harness kept its own session.
    expect(second.request?.assistantTurns).toContain(firstAnswer.text)
    expect((await thread.sandboxes()).map(sandbox => sandbox.harness)).toEqual([harness])
  }, 2 * turnTimeoutMs)
}
