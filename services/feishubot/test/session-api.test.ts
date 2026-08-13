import { describe, expect, it } from 'bun:test'
import { FeishuSessionApi } from '../src/session-api.js'

function response(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' }
  })
}

describe('Feishu durable API client', () => {
  it('continues an active binding and falls back to first-task intake only on 404', async () => {
    const calls: Array<{ url: string; init: RequestInit }> = []
    const api = new FeishuSessionApi({
      baseUrl: 'http://api-rs:8080',
      apiKey: 'internal-key',
      fetch: async (input, init) => {
        calls.push({ url: String(input), init: init ?? {} })
        if (calls.length === 1) return response({ error: 'not found' }, 404)
        return response({
          thread_key: 'development:1', workspace_id: 'wsp_1',
          selection_flow_id: 'sel_1', execution_id: 'exe_1', created: true
        })
      }
    })
    const result = await api.acceptMessage({
      eventId: 'evt-1', messageId: 'om-1', tenantKey: 'tenant-1',
      chatId: 'oc-1', conversationKey: 'ou-1', rootMessageId: 'direct',
      senderOpenId: 'ou-1', principalId: 'feishu:tenant-1:on-1',
      text: 'Fix it', isDirect: true
    })
    expect(result).toMatchObject({ thread_key: 'development:1', selection_flow_id: 'sel_1' })
    expect(calls.map(call => call.url)).toEqual([
      'http://api-rs:8080/api/development/tasks/continue',
      'http://api-rs:8080/api/development/tasks'
    ])
    expect(calls[0]?.init.headers).toMatchObject({ authorization: 'Bearer internal-key' })
    expect(JSON.parse(String(calls[1]?.init.body))).toMatchObject({
      channel: { platform: 'feishu', tenant_key: 'tenant-1' },
      harness_type: 'codex',
      initiator: { principal_id: 'feishu:tenant-1:on-1' }
    })
  })

  it('uses only opaque IDs for catalog, selection, publish, and retry operations', async () => {
    const calls: Array<{ url: string; body?: unknown }> = []
    const api = new FeishuSessionApi({
      baseUrl: 'http://api-rs:8080/',
      fetch: async (input, init) => {
        calls.push({ url: String(input), body: init?.body ? JSON.parse(String(init.body)) : undefined })
        return response({ repositories: [], next_cursor: null })
      }
    })
    await api.searchRepositories('服务 api', 'cursor+/=')
    await api.confirmSelection('sel_1', 3, 'feishu:tenant:on-1', ['gitlab:42'])
    await api.confirmNoProject('sel_1', 3, 'feishu:tenant:on-1')
    await api.cancelSelection('sel_1', 3, 'feishu:tenant:on-1')
    await api.approvePublication('chg_1', 'feishu:tenant:on-1', 'approve-1')
    await api.retryPublication('pub_1', 'feishu:tenant:on-1', 'retry-1')
    expect(calls).toEqual([
      { url: 'http://api-rs:8080/api/development/repositories?query=%E6%9C%8D%E5%8A%A1+api&cursor=cursor%2B%2F%3D', body: undefined },
      { url: 'http://api-rs:8080/api/development/selections/sel_1/confirm', body: { expected_version: 3, decided_by_principal_id: 'feishu:tenant:on-1', repository_ids: ['gitlab:42'] } },
      { url: 'http://api-rs:8080/api/development/selections/sel_1/no-project', body: { expected_version: 3, decided_by_principal_id: 'feishu:tenant:on-1' } },
      { url: 'http://api-rs:8080/api/development/selections/sel_1/cancel', body: { expected_version: 3, decided_by_principal_id: 'feishu:tenant:on-1' } },
      { url: 'http://api-rs:8080/api/development/feishu/changesets/chg_1/publish', body: { requested_by_principal_id: 'feishu:tenant:on-1', idempotency_key: 'approve-1' } },
      { url: 'http://api-rs:8080/api/development/feishu/publish-batches/pub_1/retry', body: { requested_by_principal_id: 'feishu:tenant:on-1', idempotency_key: 'retry-1' } }
    ])
  })
})
