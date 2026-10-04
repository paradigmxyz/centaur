// How a turn ends, and what reaches it while it runs. Every outcome must leave
// one visible reply: an answer, a failure the provider reported, or an answer
// with no text. A mention during a turn steers it, carrying its sender's
// identity, with a reaction on each follow-up until the turn answers; replies
// without a mention neither steer nor stop it.
import { expect, test } from 'bun:test'
import { USER_B, eventually, model, slack, turnTimeoutMs, type Channel } from '../lib'

const STEERING_REACTION = 'hourglass_flowing_sand'

test.concurrent('an answer with no text still ends the turn visibly', async () => {
  const thread = await slack.mention('--codex Say nothing.', model.says(''))
  const turn = await thread.nextTurn()

  expect(turn.execution.status).toBe('completed')
  expect(turn.reply).toContain('no final text')
}, 2 * turnTimeoutMs)

test.concurrent('a provider failure shows in the reply', async () => {
  const failure = model.fails({ status: 400, type: 'invalid_request_error', message: 'e2e scripted provider failure' })
  const thread = await slack.mention('--claude Try this.', failure)
  const turn = await thread.nextTurn()

  expect(turn.reply).toContain('e2e scripted provider failure')
}, 2 * turnTimeoutMs)

test.concurrent('replies without a mention neither steer nor stop a running turn', async () => {
  const answer = model.says('Done anyway.', { delayMs: 5_000 })
  const thread = await slack.mention('--codex Work on this.', answer)
  await thread.inFlight()
  await thread.post('One more constraint, no mention.')
  await thread.post('stop')
  const turn = await thread.nextTurn()

  expect(turn.execution.status).toBe('completed')
  expect(turn.reply).toBe('Done anyway.')
  for (const request of await model.requests(answer)) {
    expect(request.prompt).not.toContain('One more constraint, no mention.')
  }
}, 2 * turnTimeoutMs)

test.concurrent('a mention during a turn steers it as its sender, marked until the answer', async () => {
  const thread = await slack.mention('--codex Start a long task.', model.says('First part.', { delayMs: 5_000 }))
  await thread.inFlight()
  const steer = model.says('Steered answer.')
  const followUp = await thread.interject('Also handle this.', steer, { as: USER_B })
  await waitForReaction(thread.channel, followUp, true)
  const turn = await thread.nextTurn()

  // The turn answers before and after taking the follow-up.
  expect(turn.reply).toContain('Steered answer.')
  const [steered] = await model.requests(steer)
  expect(steered?.userMessage).toContain('Also handle this.')
  expect(steered?.userMessage).toContain(`Slack user ID: ${USER_B.id}`)
  await waitForReaction(thread.channel, followUp, false)
}, 2 * turnTimeoutMs)

async function waitForReaction(channel: Channel, ts: string, present: boolean): Promise<void> {
  await eventually(`the steering reaction on ${ts} to be ${present ? 'added' : 'removed'}`, 60_000, async () => {
    const message = await slack.message(channel, ts)
    const reacted = (message.reactions ?? []).some(reaction => reaction.name === STEERING_REACTION)
    return reacted === present ? true : undefined
  })
}
