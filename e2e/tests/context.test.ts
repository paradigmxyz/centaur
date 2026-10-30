// What the model is given from Slack. slackbotv2 brings a thread's earlier
// messages into the first turn it is mentioned in, carries replies it was not
// mentioned in into its next turn, leaves the rest of the channel out, and runs
// a DM as one conversation whose session acts for the DM's user.
import { expect, test } from 'bun:test'
import {
  EXTERNAL_TEAM,
  EXTERNAL_USER,
  TEAM,
  USER,
  USER_B,
  ironControl,
  model,
  slack,
  turnTimeoutMs
} from '../lib'

test.concurrent('a first mention mid-thread brings the thread so far', async () => {
  const root = await slack.post('Root context for the thread.')
  await slack.post('First earlier reply.', { threadTs: root })
  await slack.post('Second earlier reply.', { threadTs: root, as: USER_B })
  const thread = await slack.mention('--codex Summarize the thread.', model.says('Summary.'), { threadTs: root })
  const turn = await thread.nextTurn()

  expect(turn.reply).toBe('Summary.')
  for (const text of ['Root context for the thread.', 'First earlier reply.', 'Second earlier reply.']) {
    expect(turn.request?.userMessage).toContain(text)
  }
}, 2 * turnTimeoutMs)

test.concurrent('replies the bot was not mentioned in reach its next turn', async () => {
  const thread = await slack.mention('--codex Start here.', model.says('Started.'))
  await thread.nextTurn()
  await thread.post('A detail added without a mention.', { as: USER_B })
  await thread.mention('Now use it.', model.says('Used.'))
  const second = await thread.nextTurn()

  expect(second.reply).toBe('Used.')
  expect(second.request?.userMessage).toContain('A detail added without a mention.')
  // The bot's own earlier answer is the harness's memory, not new Slack context.
  expect(second.request?.assistantTurns).toContain('Started.')
  expect(second.request?.userMessage).not.toContain('Started.')
}, 3 * turnTimeoutMs)

test.concurrent('a mention that starts a thread leaves earlier channel messages out', async () => {
  await slack.post('Unrelated channel chatter.')
  const thread = await slack.mention('--codex Answer only this.', model.says('Answered.'))
  const turn = await thread.nextTurn()

  expect(turn.reply).toBe('Answered.')
  expect(turn.request?.prompt).not.toContain('Unrelated channel chatter.')
}, 2 * turnTimeoutMs)

test.concurrent('a DM is one conversation that acts for its user', async () => {
  const dm = await slack.dm('Hello there.', model.says('Hi.'))
  const first = await dm.nextTurn()
  await dm.mention('And again.', model.says('Hi again.'))
  const second = await dm.nextTurn()

  expect([first.reply, second.reply]).toEqual(['Hi.', 'Hi again.'])
  expect(second.sandbox.id).toBe(first.sandbox.id)
  expect(second.request?.assistantTurns).toContain('Hi.')
  const proxy = await ironControl.proxy(second.sandbox.id)
  expect(proxy.principal).toMatchObject({ kind: 'slack_dm', slackUserId: USER.id, slackTeamId: TEAM.id, slackEmail: USER.email })
  expect(proxy.requester).toBeNull()
}, 3 * turnTimeoutMs)

test.concurrent("a Slack Connect partner's DM keeps their email out of iron-control", async () => {
  const dm = await slack.dm('Hello from the partner org.', model.says('Hello, partner.'), { as: EXTERNAL_USER })
  const turn = await dm.nextTurn()

  expect(turn.reply).toBe('Hello, partner.')
  const proxy = await ironControl.proxy(turn.sandbox.id)
  expect(proxy.principal).toMatchObject({ kind: 'slack_dm', slackUserId: EXTERNAL_USER.id, slackTeamId: EXTERNAL_TEAM.id, slackEmail: null })
}, 2 * turnTimeoutMs)
