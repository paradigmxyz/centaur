// A user starts a Slack thread with each harness and follows up in it. Both
// answers must reach the thread once, the harness must still hold the first
// exchange in its own session, the proxy must swap in the real provider key,
// and the thread must stay on one session and one sandbox.
import { expect, test } from 'bun:test'
import {
  botReplies,
  mention,
  model,
  providerCredentials,
  sessionEvents,
  turnTimeoutMs,
  waitForExecutions,
  waitForReply
} from './lib'

for (const harness of ['codex', 'claudecode']) {
  test.concurrent(`${harness} answers and remembers a Slack thread`, async () => {
    const first = await model.reply('First answer.')
    const second = await model.reply('Second answer.')

    const root = await mention(`--${harness} Start the thread. ${first}`)
    await waitForExecutions(root, 1)
    await waitForReply(root, text => text.includes('First answer.'))

    await mention(`Follow up. ${second}`, root)
    const executions = await waitForExecutions(root, 2)
    await waitForReply(root, text => text.includes('Second answer.'))

    expect(executions.map(execution => execution.status)).toEqual(['completed', 'completed'])
    const answers = (await botReplies(root)).map(reply => reply.text ?? '')
    expect(answers.filter(text => text.includes('First answer.'))).toHaveLength(1)
    expect(answers.filter(text => text.includes('Second answer.'))).toHaveLength(1)

    const [firstRequest] = await model.requests(first)
    const [secondRequest] = await model.requests(second)
    for (const request of [firstRequest, secondRequest]) {
      expect(request?.credential).toBe(providerCredentials[harness])
    }
    // slackbotv2 quotes earlier Slack messages into each turn, so only an
    // assistant turn in the request proves the harness kept its own session.
    const history: Array<{ role?: string; content?: unknown }> =
      secondRequest?.body.input ?? secondRequest?.body.messages ?? []
    const priorAnswers = history
      .filter(item => item.role === 'assistant')
      .map(item => JSON.stringify(item.content))
    expect(priorAnswers).toContainEqual(expect.stringContaining('First answer.'))

    const ready = await sessionEvents(root, 'session.sandbox_ready')
    expect(new Set(ready.map(event => event.harness_type))).toEqual(new Set([harness]))
    expect(new Set(ready.map(event => event.sandbox_id)).size).toBe(1)
  }, 2 * turnTimeoutMs)
}
