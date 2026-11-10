// A sandbox reaches a remote development environment over SSH only through
// its paired iron-proxy, with an SSH key an operator granted to the
// conversation's principal. The sandbox holds only a disposable key; the
// granted key logs in upstream, only at destinations the deployment's policy
// allows, and only while the grant stands: revoking it refuses new connections
// and ends live ones at the proxy's next renewal.
//
// One thread covers it all, so the scenario holds a single sandbox and proxy
// on the shared kind node, and pauses the sandbox when it is done.
import { afterAll, beforeAll, expect, test } from 'bun:test'
import { randomUUID } from 'node:crypto'
import { api, eventually, ironControl, model, slack, sshGateway, turnTimeoutMs, type Thread } from '../lib'

const consoleApi = ironControl.api
// Marks the keys this file creates, so they can be found and removed.
const LABELS = { e2e: 'ssh' }
// What ssh exits with when the proxy refuses the connection.
const REFUSED = '255'

// Deleting a key also deletes its grants, so a failed run leaves no access behind.
async function removeKeys(): Promise<void> {
  const { data } = await consoleApi.ok('GET', '/api/v1/ssh_key_secrets?labels[e2e]=ssh&limit=200')
  for (const key of data) await consoleApi.ok('DELETE', `/api/v1/ssh_key_secrets/${key.id}`)
}

beforeAll(removeKeys)
afterAll(removeKeys)

/** Stores the gateway's authorized key as an operator key; returns its id and public key. */
async function createKey(): Promise<{ id: string; publicKey: string }> {
  const { data } = await consoleApi.ok('POST', '/api/v1/ssh_key_secrets', {
    data: {
      foreign_id: `e2e-ssh-${randomUUID().slice(0, 8)}`,
      name: 'E2E remote workspace key',
      labels: LABELS,
      private_key: { source_type: 'control_plane', secret: sshGateway.privateKey, config: {} }
    }
  })
  return { id: data.id, publicKey: data.public_key }
}

/** Grants the key to the principal of the thread's sandbox proxy; returns the grant id. */
async function grantToConversation(thread: Thread, keyId: string): Promise<string> {
  const [sandbox] = await thread.sandboxes()
  const { principal } = await ironControl.proxy(sandbox!.id)
  const { data: record } = await consoleApi.ok(
    'GET',
    `/api/v1/principals/lookup/${encodeURIComponent(principal.foreignId)}`
  )
  const { data: grant } = await consoleApi.ok('POST', '/api/v1/grants', {
    data: { principal_id: record.id, ssh_key_secret_id: keyId }
  })
  return grant.id
}

/** `name=value` lines a command printed, keyed by name. */
function fields(output: string): Record<string, string> {
  return Object.fromEntries([...output.matchAll(/^(\w+)=(.*)$/gm)].map(([, name, value]) => [name, value!.trim()]))
}

test('a granted SSH key reaches allowed destinations until revoked, ending live connections', async () => {
  const marker = `e2e-${randomUUID().slice(0, 8)}`
  await sshGateway.exec(`echo ${marker} > /workspace/${marker} && chown dev /workspace/${marker}`)
  const remoteCat = `remote-ssh exec --url ${sshGateway.url} -- cat ${marker}; echo "exit=$?"`

  // No key is granted yet, so the proxy refuses the connection.
  const denied = model.runs(remoteCat, 'Denied.')
  const thread = await slack.mention('--codex Read the remote marker.', denied)
  expect((await thread.nextTurn()).reply).toBe('Denied.')
  expect(fields(await model.commandOutput(denied)).exit).toBe(REFUSED)

  const key = await createKey()
  let grantId = await grantToConversation(thread, key.id)

  // Authorization is checked per connection, so the grant applies at once.
  const granted = model.runs(
    [
      `remote-ssh exec --url ${sshGateway.url} -- sh -c 'echo "who=$(id -un)"; echo "cwd=$(pwd)"; echo "marker=$(cat ${marker})"'`,
      `echo "exec_exit=$?"`,
      `printf written-${marker} | remote-ssh write --url ${sshGateway.url} written-${marker}; echo "write_exit=$?"`,
      `echo "read=$(remote-ssh read --url ${sshGateway.url} written-${marker})"`,
      // Destinations outside the deployment's policy are refused despite the grant.
      `remote-ssh exec --url wss://outside.example.test/events/events -- true; echo "other_host_exit=$?"`,
      `remote-ssh exec --url wss://workspace.ssh-gateway.test/other -- true; echo "other_path_exit=$?"`,
      // The public half of the only key the sandbox holds.
      `python3 -c 'import json, os; print(json.loads(os.environ["CENTAUR_SSH"])["private_key"])' > /tmp/disposable-key`,
      `chmod 600 /tmp/disposable-key; echo "sandbox_key=$(ssh-keygen -y -f /tmp/disposable-key | cut -d' ' -f1-2)"`
    ].join('\n'),
    'Connected.'
  )
  await thread.mention('Read it again.', granted)
  expect((await thread.nextTurn()).reply).toBe('Connected.')
  const output = fields(await model.commandOutput(granted))
  expect(output).toMatchObject({
    who: 'dev',
    cwd: '/workspace',
    marker,
    exec_exit: '0',
    write_exit: '0',
    read: `written-${marker}`,
    other_host_exit: REFUSED,
    other_path_exit: REFUSED
  })
  expect(await sshGateway.exec(`cat /workspace/written-${marker}`)).toBe(`written-${marker}`)
  // The granted key logged in upstream; the sandbox never held it.
  expect(output.sandbox_key).toMatch(/^ssh-ed25519 /)
  expect(output.sandbox_key).not.toBe(key.publicKey.split(' ').slice(0, 2).join(' '))

  await consoleApi.ok('DELETE', `/api/v1/grants/${grantId}`)
  const revoked = model.runs(remoteCat, 'Denied again.')
  await thread.mention('Read it once more.', revoked)
  expect((await thread.nextTurn()).reply).toBe('Denied again.')
  expect(fields(await model.commandOutput(revoked)).exit).toBe(REFUSED)

  // A live connection ends once its grant is revoked.
  grantId = await grantToConversation(thread, key.id)
  // A detached connection outlives the turn. Its remote loop records its pid,
  // then writes until the connection closes and the write fails.
  const pidFile = `/workspace/live-${randomUUID().slice(0, 8)}.pid`
  const loop = `echo $$ > ${pidFile}; while echo tick; do sleep 1; done`
  const connect = model.runs(
    `setsid nohup remote-ssh exec --url ${sshGateway.url} -- sh -c '${loop}' > /dev/null 2>&1 < /dev/null &`,
    'Connected.'
  )
  await thread.mention('Open a connection.', connect)
  expect((await thread.nextTurn()).reply).toBe('Connected.')

  const alive = async () => (await sshGateway.exec(`kill -0 "$(cat ${pidFile})" 2>/dev/null && echo alive || echo gone`)) === 'alive'
  await eventually('the remote loop to start', 60_000, async () => ((await alive()) ? true : undefined))
  // The proxy renews the lease every five seconds; the connection survives renewals while granted.
  await Bun.sleep(12_000)
  expect(await alive()).toBe(true)

  await consoleApi.ok('DELETE', `/api/v1/grants/${grantId}`)
  await eventually('the revoked connection to close', 30_000, async () => ((await alive()) ? undefined : true))

  // Free the sandbox's pod for the scenarios still running on the node.
  await api.pause(thread)
}, 6 * turnTimeoutMs)
