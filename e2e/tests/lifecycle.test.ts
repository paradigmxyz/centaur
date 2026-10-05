// Sandbox lifecycle through real Kubernetes, seen from a Slack thread: pause
// and resume, a sandbox killed between turns, an api-rs restart, and cleanup
// after a harness switch. These disrupt shared components, so they run one at
// a time rather than alongside other scenarios.
import { expect, test } from 'bun:test'
import { api, cluster, eventually, model, slack, turnTimeoutMs } from '../lib'

test('a paused thread resumes in the same sandbox on its next turn', async () => {
  const first = model.says('First answer.')
  const second = model.says('Second answer.')
  const thread = await slack.mention('--codex Start the thread.', first)
  const before = await thread.nextTurn()

  expect(await api.pause(thread)).toBe(true)
  // Sandbox pods exit on SIGTERM, so a pause frees the pod within seconds.
  await eventually('the paused sandbox to release its pod', 15_000, async () =>
    (await cluster.sandboxResources(before.sandbox.id)).some(name => name === `pod/${before.sandbox.id}`)
      ? undefined
      : true
  )

  await thread.mention('Follow up.', second)
  const after = await thread.nextTurn()
  expect(after.reply).toBe(second.text)
  expect(after.sandbox).toEqual({ ...before.sandbox, source: 'resumed' })
}, 3 * turnTimeoutMs)

test('a thread is answered after its sandbox pod is killed', async () => {
  const first = model.says('First answer.')
  const second = model.says('Second answer.')
  const thread = await slack.mention('--codex Start the thread.', first)
  const before = await thread.nextTurn()

  await cluster.killSandbox(before.sandbox.id)
  await thread.mention('Follow up.', second)
  const after = await thread.nextTurn()
  expect(after.reply).toBe(second.text)
}, 3 * turnTimeoutMs)

test('a thread carries on in the same sandbox after an api-rs restart', async () => {
  const first = model.says('First answer.')
  const second = model.says('Second answer.')
  const thread = await slack.mention('--codex Start the thread.', first)
  const before = await thread.nextTurn()

  await cluster.restart('api-rs')
  await thread.mention('Follow up.', second)
  const after = await thread.nextTurn()
  expect(after.reply).toBe(second.text)
  expect(after.sandbox.id).toBe(before.sandbox.id)
  expect(after.request?.assistantTurns).toContain(first.text)
}, 3 * turnTimeoutMs)

test('a harness switch removes the old sandbox and its proxy', async () => {
  const first = model.says('First answer.')
  const second = model.says('Second answer.')
  const thread = await slack.mention('--codex Start the thread.', first)
  const before = await thread.nextTurn()
  // Guard against a vacuous pass: the lookup must see what the sandbox owns.
  const owned = await cluster.sandboxResources(before.sandbox.id)
  expect(owned).toContain(`pod/${before.sandbox.id}`)
  expect(owned).toContainEqual(expect.stringMatching(new RegExp(`^pod/${before.sandbox.id}-proxy-`)))
  expect(owned).toContain(`service/${before.sandbox.id}-proxy`)
  expect(owned.filter(name => name.startsWith('networkpolicy'))).toHaveLength(2)

  await thread.mention('--claude Switch.', second)
  const after = await thread.nextTurn()
  expect(after.reply).toBe(second.text)
  expect(after.sandbox.id).not.toBe(before.sandbox.id)
  await eventually('the old sandbox to be cleaned up', 15_000, async () => {
    const left = await cluster.sandboxResources(before.sandbox.id)
    return left.length === 0 ? true : undefined
  })
}, 3 * turnTimeoutMs)
