// Credential changes an operator makes in iron-control reach a running
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

const SECRET_PREFIX = 'e2e-credential-swap-'

// A failed run must not leave its grant on the shared channel principal.
async function removeSwappedSecrets(): Promise<void> {
  await ironControl.rails(`StaticSecret.where("foreign_id LIKE '${SECRET_PREFIX}%'").destroy_all`)
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
    const { principal } = await ironControl.proxy(first.sandbox.id)

    const secret = `${SECRET_PREFIX}${randomUUID().slice(0, 8)}`
    const credentialAfter = async (change: string): Promise<string | undefined> => {
      await ironControl.rails(change)
      await waitForSyncsAfterChange(first.sandbox.id)
      await thread.mention('Call the model again.', model.says('Called.'))
      const turn = await thread.nextTurn()
      expect(turn.sandbox.id).toBe(first.sandbox.id)
      return turn.request?.credential
    }

    // A direct grant outranks the infra role's provider key on the same
    // host and header, so deconfliction withholds the provider key.
    const granted = `sk-e2e-granted-${randomUUID().slice(0, 8)}`
    expect(await credentialAfter(`
      principal = Principal.find_by!(foreign_id: ${JSON.stringify(principal.foreignId)})
      provider_key = StaticSecret.where("replace_config->>'proxy_value' = 'OPENAI_API_KEY'").sole
      secret = StaticSecret.new(
        foreign_id: ${JSON.stringify(secret)}, created_by: principal.created_by,
        inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" })
      secret.build_source(source_type: "control_plane", secret: ${JSON.stringify(granted)})
      provider_key.rules.each { |rule| secret.rules.build(host: rule.host, position: rule.position) }
      secret.save!
      Grant.create!(principal: principal, static_secret: secret, created_by: principal.created_by)
    `)).toBe(`Bearer ${granted}`)

    const rotated = `sk-e2e-rotated-${randomUUID().slice(0, 8)}`
    expect(await credentialAfter(`
      StaticSecret.find_by!(foreign_id: ${JSON.stringify(secret)}).source.update!(secret: ${JSON.stringify(rotated)})
    `)).toBe(`Bearer ${rotated}`)

    expect(await credentialAfter(`
      StaticSecret.find_by!(foreign_id: ${JSON.stringify(secret)}).destroy!
    `)).toBe(providerCredentials.openai)
  },
  8 * turnTimeoutMs
)
