// A user starts a Slack thread with codex and follows up in it. Both answers
// must reach the thread once, the follow-up must carry the first exchange to
// the model, the proxy must swap in the real key, and the thread must stay on
// one session and one sandbox.
import { expect, test } from 'bun:test'
import {
  botReplies,
  mention,
  model,
  openaiKey,
  sessionEvents,
  turnTimeoutMs,
  waitForExecutions,
  waitForReply
} from './lib'

test('codex answers and remembers a Slack thread', async () => {
  const first = await model.reply('First answer.')
  const second = await model.reply('Second answer.')

  const root = await mention(`--codex Start the thread. ${first}`)
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
    expect(request?.authorization).toBe(`Bearer ${openaiKey}`)
  }
  const history = JSON.stringify(secondRequest?.body.input)
  expect(history).toContain(`Start the thread. ${first}`)
  expect(history).toContain('First answer.')

  const ready = await sessionEvents(root, 'session.sandbox_ready')
  expect(new Set(ready.map(event => event.harness_type))).toEqual(new Set(['codex']))
  expect(new Set(ready.map(event => event.sandbox_id)).size).toBe(1)
}, 2 * turnTimeoutMs)
