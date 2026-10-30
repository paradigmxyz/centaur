// Credential changes an operator makes through the console API reach a running
// sandbox through proxy-sync on its next syncs, without restarting it. The
// sandbox sees them as a tool would: the model has the harness curl an echo
// path that records the header iron-proxy injected. A granted secret is
// injected, a rotated value replaces it, and revoking the grant removes it.
import { afterAll, expect, test } from 'bun:test'
import { randomUUID } from 'node:crypto'
import {
  cluster,
  eventually,
  ironControl,
  model,
  slack,
  turnTimeoutMs,
  type Script,
  type Thread
} from '../lib'

const consoleApi = ironControl.api
// The model server serves the echo path on the provider hosts.
const ECHO_HOST = 'api.openai.com'
// Marks the secrets this file creates, so they can be found and removed.
const LABELS = { e2e: 'credential-swap' }

// A failed run must not leave its grant on the channel principal.
async function removeSwappedSecrets(): Promise<void> {
  const { data } = await consoleApi.ok('GET', '/api/v1/static_secrets?labels[e2e]=credential-swap&limit=200')
  for (const secret of data) await consoleApi.ok('DELETE', `/api/v1/static_secrets/${secret.id}`)
}

afterAll(removeSwappedSecrets)

/**
 * Waits until the proxy finishes two syncs after the change committed. The
 * first may have started before the commit; the second cannot have.
 */
async function waitForSyncsAfterChange(sandboxId: string): Promise<void> {
  const seen = new Set([(await cluster.proxyStatus(sandboxId)).last_sync_at])
  await eventually(`two syncs of ${sandboxId}'s proxy after the change`, 60_000, async () => {
    seen.add((await cluster.proxyStatus(sandboxId)).last_sync_at)
    return seen.size > 2 ? true : undefined
  })
}

test('granted, rotated, and revoked credentials reach a running sandbox', async () => {
  await removeSwappedSecrets()

  /** Runs a turn whose model curls the echo path; returns the credential it carried. */
  const echoedCredential = async (send: (script: Script) => Promise<Thread>): Promise<string | undefined> => {
    const match = randomUUID().slice(0, 8)
    const script = model.runs(`curl -fsS https://${ECHO_HOST}/_e2e/echo/${match}`, 'Checked.')
    const turn = await (await send(script)).nextTurn()
    expect(turn.reply).toBe('Checked.')
    const echoes = await model.echoes(match)
    expect(echoes).toHaveLength(1)
    return echoes[0]
  }

  let thread: Thread | undefined
  expect(await echoedCredential(async script => {
    thread = await slack.mention('--codex Check the credential.', script)
    return thread
  })).toBeUndefined()
  const [sandbox] = await thread!.sandboxes()
  const credentialAfter = async (change: () => Promise<unknown>): Promise<string | undefined> => {
    await change()
    await waitForSyncsAfterChange(sandbox!.id)
    const credential = await echoedCredential(async script => {
      await thread!.mention('Check it again.', script)
      return thread!
    })
    expect((await thread!.sandboxes()).map(({ id }) => id)).toEqual([sandbox!.id])
    return credential
  }

  const foreignId = `e2e-credential-swap-${randomUUID().slice(0, 8)}`
  const upsertSecret = (value: string) =>
    consoleApi.ok('PUT', `/api/v1/static_secrets/${foreignId}`, {
      data: {
        labels: LABELS,
        inject_config: { header: 'X-E2E-Credential' },
        source: { source_type: 'control_plane', secret: value, config: {} },
        rules: [{ host: ECHO_HOST, paths: ['/_e2e/echo/*'] }]
      }
    })
  const { principal } = await ironControl.proxy(sandbox!.id)
  const { data: principalRecord } = await consoleApi.ok(
    'GET',
    `/api/v1/principals/lookup/${encodeURIComponent(principal.foreignId)}`
  )

  const granted = `e2e-granted-${randomUUID().slice(0, 8)}`
  let grantId = ''
  expect(await credentialAfter(async () => {
    const { data: secret } = await upsertSecret(granted)
    const { data: grant } = await consoleApi.ok('POST', '/api/v1/grants', {
      data: { principal_id: principalRecord.id, static_secret_id: secret.id }
    })
    grantId = grant.id
  })).toBe(granted)

  const rotated = `e2e-rotated-${randomUUID().slice(0, 8)}`
  expect(await credentialAfter(() => upsertSecret(rotated))).toBe(rotated)

  expect(await credentialAfter(() => consoleApi.ok('DELETE', `/api/v1/grants/${grantId}`))).toBeUndefined()
}, 8 * turnTimeoutMs)
