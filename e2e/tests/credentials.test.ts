// Credential changes an operator makes through the console API reach a running
// sandbox's proxy through proxy-sync on its next syncs, without restarting the
// sandbox: a granted secret that outranks the provider key replaces it, a
// rotated value is picked up, and revoking the grant restores the provider key.
import { afterAll, expect, test } from 'bun:test'
import { randomUUID } from 'node:crypto'
import {
  CREDENTIALS_CHANNEL,
  authMode,
  cluster,
  eventually,
  ironControl,
  model,
  providerCredentials,
  slack,
  turnTimeoutMs
} from '../lib'

const consoleApi = ironControl.api
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

// The swap replaces Codex's API key, which only api_key mode sends.
test.skipIf(authMode !== 'api_key')(
  'granted, rotated, and revoked credentials reach a running sandbox',
  async () => {
    await removeSwappedSecrets()
    const thread = await slack.mention('--codex Use the provider key.', model.says('Provider key.'), {
      channel: CREDENTIALS_CHANNEL
    })
    const first = await thread.nextTurn()
    expect(first.request?.credential).toBe(providerCredentials.openai)

    const credentialAfter = async (change: () => Promise<unknown>): Promise<string | undefined> => {
      await change()
      await waitForSyncsAfterChange(first.sandbox.id)
      await thread.mention('Call the model again.', model.says('Called.'))
      const turn = await thread.nextTurn()
      expect(turn.sandbox.id).toBe(first.sandbox.id)
      return turn.request?.credential
    }

    // Inject on the hosts the infra role's OpenAI key covers.
    const { data: secrets } = await consoleApi.ok('GET', '/api/v1/static_secrets?limit=200')
    const providerKey = secrets.find((secret: any) => secret.replace_config?.proxy_value === 'OPENAI_API_KEY')
    if (!providerKey) throw new Error('the console has no OPENAI_API_KEY replace secret')
    const rules = providerKey.rules.map(({ host, cidr, http_methods, paths }: any) =>
      Object.fromEntries(Object.entries({ host, cidr, http_methods, paths }).filter(([, value]) => value != null))
    )
    const foreignId = `e2e-credential-swap-${randomUUID().slice(0, 8)}`
    const upsertSecret = (value: string) =>
      consoleApi.ok('PUT', `/api/v1/static_secrets/${foreignId}`, {
        data: {
          labels: LABELS,
          inject_config: { header: 'Authorization', formatter: 'Bearer {{ .Value }}' },
          source: { source_type: 'control_plane', secret: value, config: {} },
          rules
        }
      })

    // A direct grant outranks the infra role's grant on the same host and
    // header, so deconfliction withholds the provider key.
    const { principal } = await ironControl.proxy(first.sandbox.id)
    const { data: principalRecord } = await consoleApi.ok(
      'GET',
      `/api/v1/principals/lookup/${encodeURIComponent(principal.foreignId)}`
    )
    const granted = `sk-e2e-granted-${randomUUID().slice(0, 8)}`
    let grantId = ''
    expect(await credentialAfter(async () => {
      const { data: secret } = await upsertSecret(granted)
      const { data: grant } = await consoleApi.ok('POST', '/api/v1/grants', {
        data: { principal_id: principalRecord.id, static_secret_id: secret.id }
      })
      grantId = grant.id
    })).toBe(`Bearer ${granted}`)

    const rotated = `sk-e2e-rotated-${randomUUID().slice(0, 8)}`
    expect(await credentialAfter(() => upsertSecret(rotated))).toBe(`Bearer ${rotated}`)

    expect(await credentialAfter(() => consoleApi.ok('DELETE', `/api/v1/grants/${grantId}`)))
      .toBe(providerCredentials.openai)
  },
  8 * turnTimeoutMs
)
