// A nanocode turn can check out a hand, a single-use Linux box from api-rs's
// warm pool, run a command on it as the thread's channel principal, and release
// it. The lease is a durable hand_boxes row; the model sees the checkout and
// command results as tool outputs; a credential granted to the channel
// principal is injected by the hand's own iron-proxy sidecar; and release
// deletes the pod, drops the row, and lets the pool refill.
import { afterAll, expect, test } from 'bun:test'
import { randomUUID } from 'node:crypto'
import {
  CHANNEL,
  cluster,
  db,
  eventually,
  hands,
  ironControl,
  model,
  releaseSandboxesAfterAll,
  slack,
  turnTimeoutMs
} from '../lib'

releaseSandboxesAfterAll()

const consoleApi = ironControl.api
// The fake model server serves the echo path on the provider hosts.
const ECHO_HOST = 'api.openai.com'
// Marks the secrets this file creates, so they can be found and removed.
const LABELS = { e2e: 'hands' }

async function removeSwappedSecrets(): Promise<void> {
  const { data } = await consoleApi.ok('GET', '/api/v1/static_secrets?labels[e2e]=hands&limit=200')
  for (const secret of data) await consoleApi.ok('DELETE', `/api/v1/static_secrets/${secret.id}`)
}

afterAll(removeSwappedSecrets)

test.concurrent('a nanocode turn checks out a hand, runs a command on it, and releases it', async () => {
  await removeSwappedSecrets()
  // A first turn creates the conversation's session and channel principal, so
  // the credential below can be granted before the hand turn starts.
  const thread = await slack.mention('--nanocode Say hello.', model.says('Hello.'))
  expect((await thread.nextBrainTurn()).reply).toBe('Hello.')

  const marker = `e2e-${randomUUID().slice(0, 8)}`
  const echo = randomUUID().slice(0, 8)
  const credential = `e2e-hand-credential-${randomUUID().slice(0, 8)}`

  // A static secret granted to the channel principal: the hand's proxy is bound
  // to that principal when it is leased, and substitutes this value on the wire.
  const foreignId = `e2e-hand-${randomUUID().slice(0, 8)}`
  const { data: secret } = await consoleApi.ok('PUT', `/api/v1/static_secrets/${foreignId}`, {
    data: {
      labels: LABELS,
      inject_config: { header: 'X-E2E-Credential' },
      source: { source_type: 'control_plane', secret: credential, config: {} },
      rules: [{ host: ECHO_HOST, paths: ['/_e2e/echo/*'] }]
    }
  })
  const principalId = await ironControl.slackChannelPrincipalId(CHANNEL.id)
  const { data: grant } = await consoleApi.ok('POST', '/api/v1/grants', {
    data: { principal_id: principalId, static_secret_id: secret.id }
  })

  // Checkout, then a shell command whose cell runs on the hand, then release.
  // The release is held back so the test can observe the live lease.
  const script = model.sequence(
    [
      { tool: 'centaur_hand_checkout' },
      {
        run: [
          `printf 'hand-marker=${marker}\\n'`,
          // Retry briefly so a proxy sync that lands just after checkout still
          // gets a request carrying the granted credential.
          `for _ in 1 2 3 4 5; do curl -fsS https://${ECHO_HOST}/_e2e/echo/${echo} || true; sleep 1; done`
        ].join('\n')
      },
      { tool: 'centaur_hand_release', delayMs: 15_000 }
    ],
    'Hand released.'
  )

  await thread.mention('Check out a hand and run a command on it.', script)
  const running = thread.nextBrainTurn()
  // Observe the lease while the release step is still held back; a failure in
  // the turn must not surface as an unhandled rejection while we poll.
  running.catch(() => {})
  const lease = await eventually(`a hand leased to ${thread.key}`, turnTimeoutMs, () => hands.lease(thread))

  expect(lease.state).toBe('leased')
  expect(lease.size).toBe('default')
  expect(lease.lease_id).toStartWith('hls_')
  expect(lease.principal).toBeTruthy()
  expect(lease.pod_ip).toBeTruthy()
  const leasedPod = await cluster.pod(lease.pod_name)
  expect(leasedPod?.labels['centaur.ai/workload']).toBe('hand')
  expect(leasedPod?.labels['centaur.ai/hand-id']).toBe(lease.id)

  const turn = await running
  expect(turn.reply).toBe('Hand released.')
  expect(turn.execution.status).toBe('completed')

  // The model received the checkout description and the command's output.
  const outputs = await model.toolOutputs(script)
  expect(outputs.some(output => output.includes(`Hand \`${lease.id}\` is checked out`))).toBe(true)
  expect(outputs.some(output => output.includes(marker))).toBe(true)

  // The command went out through the hand's sidecar proxy, which substituted
  // the channel principal's granted credential.
  await eventually(`the hand's proxy to inject the granted credential`, 60_000, async () => {
    const seen = await model.echoes(echo)
    return seen.includes(credential) ? seen : undefined
  })

  // Release deletes the pod and the durable row.
  await eventually(`hand pod ${lease.pod_name} to be deleted`, turnTimeoutMs, async () =>
    (await cluster.pod(lease.pod_name)) === undefined ? true : undefined
  )
  await eventually(`hand lease ${lease.lease_id} to be released`, turnTimeoutMs, async () =>
    (await hands.lease(thread)) === undefined ? true : undefined
  )

  // The pool replaced the leased hand with one new ready default-size hand.
  const ready = await eventually('the default hand pool to refill', turnTimeoutMs, async () => {
    const boxes = await hands.boxes('ready')
    return boxes.length === 1 ? boxes : undefined
  })
  expect(ready[0]!.id).not.toBe(lease.id)

  const [row] = await db`select count(*)::int as count from hand_boxes where id = ${lease.id}`
  expect(row.count).toBe(0)

  await consoleApi.ok('DELETE', `/api/v1/grants/${grant.id}`)
}, 2 * turnTimeoutMs)
