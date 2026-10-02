// Mentions the bot in the fake Slack once per harness and waits for each real
// harness's answer to come back through slackbotv2. Run with `just e2e`,
// which deploys the stack and port-forwards the fake Slack to E2E_SLACK_URL.
import { randomUUID } from 'node:crypto'
import { expect, test } from 'bun:test'
import { BOT, CHANNEL, USER_TOKEN } from './fixture'

type SlackMessage = { ts: string; text?: string; bot_id?: string; streaming?: boolean }

const slackUrl = process.env.E2E_SLACK_URL ?? 'http://127.0.0.1:18443'
const harnesses = (process.env.E2E_HARNESSES ?? 'codex,claudecode')
  .split(',')
  .map(harness => harness.trim())
  .filter(Boolean)
const turnTimeoutMs = Number(process.env.E2E_TURN_TIMEOUT_MS ?? 600_000)

for (const harness of harnesses) {
  test.concurrent(`${harness} answers a Slack mention`, async () => {
    const nonce = `centaur-e2e-${harness}-${randomUUID().slice(0, 8)}`
    const root = await slack<{ ts: string }>('chat.postMessage', {
      channel: CHANNEL.id,
      text: `<@${BOT.userId}> --${harness} This is an automated end-to-end test. ` +
        `Do not use any tools. Reply with exactly this text and nothing else: ${nonce}`
    })

    // Wait for the turn's reply to finish streaming, then check its content.
    const deadline = Date.now() + turnTimeoutMs
    let replies: SlackMessage[] = []
    while (Date.now() < deadline) {
      const thread = await slack<{ messages: SlackMessage[] }>('conversations.replies', {
        channel: CHANNEL.id,
        ts: root.ts
      })
      replies = thread.messages.filter(message => message.bot_id === BOT.id)
      if (replies.length > 0 && replies.every(message => !message.streaming)) break
      await Bun.sleep(2_000)
    }
    expect(replies.map(message => message.text)).toContainEqual(expect.stringContaining(nonce))
  }, turnTimeoutMs + 30_000)
}

async function slack<T>(method: string, body: Record<string, string>): Promise<T> {
  const response = await fetch(`${slackUrl}/api/${method}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${USER_TOKEN}`, 'content-type': 'application/json' },
    body: JSON.stringify(body)
  })
  const payload = await response.json() as { ok: boolean; error?: string } & T
  if (!payload.ok) throw new Error(`${method} failed: ${payload.error}`)
  return payload
}
