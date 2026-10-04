// Who a Slack turn runs as. slackbotv2 resolves the sender's Slack profile into
// the requester context the model sees, and api-rs binds the sender to the
// sandbox's proxy in iron-control as its requester, so credentials act for the
// person who asked rather than whoever started the thread. A Slack Connect
// partner's turn binds no requester, and iron-control gets no principal (and
// so no email) for them.
import { expect, test } from 'bun:test'
import {
  EXTERNAL_USER,
  TEAM,
  USER,
  USER_B,
  ironControl,
  model,
  slack,
  turnTimeoutMs,
  type SlackUser
} from '../lib'

/** The requester iron-control should bind for a home-workspace user. */
function requester(user: SlackUser) {
  return { slackUserId: user.id, slackTeamId: TEAM.id, slackEmail: user.email }
}

test.concurrent('a mention runs as the Slack user who sent it', async () => {
  const thread = await slack.mention('--codex Who am I?', model.says('Hello.'))
  const turn = await thread.nextTurn()

  const message = turn.request?.userMessage
  expect(message).toContain(`Slack user ID: ${USER.id}`)
  expect(message).toContain(`GitHub handle from Slack profile: @${USER.github}`)
  expect(message).toContain(`Prompted by: @${USER.github}`)
  const proxy = await ironControl.proxy(turn.sandbox.id)
  expect(proxy.principal).toMatchObject({ kind: 'slack_channel', name: 'Slack Channel #centaur-e2e' })
  expect(proxy.requester).toMatchObject(requester(USER))
}, 2 * turnTimeoutMs)

test.concurrent('a reply from another user runs as that user', async () => {
  const thread = await slack.mention('--codex Start this.', model.says('Started.'))
  const first = await thread.nextTurn()
  await thread.mention('Now I will take it from here.', model.says('Continuing.'), { as: USER_B })
  const second = await thread.nextTurn()

  // USER_B has no GitHub handle on their profile, so attribution falls back
  // to their Slack display name.
  const message = second.request?.userMessage
  expect(message).toContain(`Slack user ID: ${USER_B.id}`)
  expect(message).toContain('GitHub handle from Slack profile: unavailable')
  expect(message).toContain(`Prompted by: ${USER_B.displayName}`)
  expect(message).not.toContain(`@${USER.github}`)
  expect(second.sandbox.id).toBe(first.sandbox.id)
  expect((await ironControl.proxy(second.sandbox.id)).requester).toMatchObject(requester(USER_B))
}, 3 * turnTimeoutMs)

test.concurrent('a reply from a Slack Connect partner runs without a requester', async () => {
  const thread = await slack.mention('--codex Start this.', model.says('Started.'))
  const first = await thread.nextTurn()
  expect((await ironControl.proxy(first.sandbox.id)).requester).toMatchObject(requester(USER))

  await thread.mention('Adding our side.', model.says('Noted.'), { as: EXTERNAL_USER })
  const second = await thread.nextTurn()

  expect(second.request?.userMessage).toContain(`Slack user ID: ${EXTERNAL_USER.id}`)
  expect(second.sandbox.id).toBe(first.sandbox.id)
  expect((await ironControl.proxy(second.sandbox.id)).requester).toBeNull()
  expect(await ironControl.slackUser(EXTERNAL_USER)).toBeUndefined()
}, 3 * turnTimeoutMs)
