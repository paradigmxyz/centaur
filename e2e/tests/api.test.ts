// The session API's harness vocabulary: every harness a client may name
// creates a session on that harness, and stale names are rejected.
import { randomUUID } from 'node:crypto'
import { expect, test } from 'bun:test'
import { api, releaseSandboxesAfterAll } from '../lib'

releaseSandboxesAfterAll()

const sessionPath = (suffix: string) =>
  `/api/session/${encodeURIComponent(`e2e-api:${randomUUID()}:${suffix}`)}`

test('each harness wire value creates a session on that harness', async () => {
  for (const harness of ['codex', 'amp', 'claudecode', 'hermes', 'pi', 'omp']) {
    const session = await api.ok('POST', sessionPath(harness), { harness_type: harness })
    expect({ harness, created: session.harness_type, status: session.status })
      .toEqual({ harness, created: harness, status: 'idle' })
  }
}, 30_000)

test('a stale harness name is rejected', async () => {
  for (const harness of ['claude-code', 'nanocodex']) {
    const { status } = await api.request('POST', sessionPath(harness), { harness_type: harness })
    expect({ harness, status }).toEqual({ harness, status: 422 })
  }
}, 30_000)
