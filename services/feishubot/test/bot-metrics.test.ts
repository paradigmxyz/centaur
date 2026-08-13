import { afterEach, describe, expect, it, spyOn } from 'bun:test'
import { FeishuBot } from '../src/bot.js'
import type { FeishuRenderer } from '../src/feishu-client.js'
import { FeishuMetrics } from '../src/metrics.js'
import { FeishuApiError, type FeishuSessionApi } from '../src/session-api.js'

const cardEvent = {
  event_id: 'evt-card-1',
  tenant_key: 'tenant-1',
  operator: { operator_id: { open_id: 'ou-user-1' } },
  action: {
    value: {
      action: 'confirm',
      selection_flow_id: 'sel_1',
      expected_version: 1
    }
  },
  context: { open_message_id: 'om-card-1' }
}

afterEach(() => {
  spyOn(console, 'error').mockRestore()
})

describe('Feishu bot metrics', () => {
  it('classifies a delivery write conflict without misclassifying it as selection', async () => {
    spyOn(console, 'error').mockImplementation(() => {})
    const metrics = new FeishuMetrics()
    const api = selectionApi({
      recordDelivery: async () => {
        throw new FeishuApiError('record delivery', 409, false)
      }
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api,
      renderer: renderer(),
      metrics
    })

    bot.acceptCardEvent(cardEvent)
    await Bun.sleep(10)

    const output = metrics.prometheus(true)
    expect(output).toContain('centaur_feishubot_stale_conflicts_total{kind="delivery"} 1')
    expect(output).not.toContain('centaur_feishubot_stale_conflicts_total{kind="selection"}')
    expect(output).toContain('centaur_feishubot_events_total{kind="card",outcome="accepted"} 1')
  })

  it('classifies a selection mutation conflict at the API boundary', async () => {
    spyOn(console, 'error').mockImplementation(() => {})
    const metrics = new FeishuMetrics()
    const api = selectionApi({
      confirmSelection: async () => {
        throw new FeishuApiError('confirm selection', 409, false)
      }
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api,
      renderer: renderer(),
      metrics
    })

    bot.acceptCardEvent(cardEvent)
    await Bun.sleep(10)

    const output = metrics.prometheus(true)
    expect(output).toContain('centaur_feishubot_stale_conflicts_total{kind="selection"} 1')
    expect(output).not.toContain('centaur_feishubot_stale_conflicts_total{kind="delivery"}')
    expect(output).toContain('centaur_feishubot_events_total{kind="card",outcome="failed"} 1')
  })
})

function selectionApi(overrides: Record<string, unknown>): FeishuSessionApi {
  return {
    getSelection: async () => ({
      selection_flow_id: 'sel_1',
      workspace_id: 'wsp_1',
      thread_key: 'development:1',
      execution_id: null,
      kind: 'initial',
      state: 'pending',
      version: 1,
      task_excerpt: 'Fix it',
      query: '',
      cursor: null,
      cursor_history: [],
      selected_repository_ids: ['gitlab:42']
    }),
    confirmSelection: async () => ({}),
    getDelivery: async () => ({
      delivery_id: 'fdl_1',
      tenant_key: 'tenant-1',
      thread_key: 'development:1',
      chat_id: 'oc-1',
      root_message_id: 'direct',
      message_id: 'om-card-1',
      last_event_cursor: 0,
      desired_version: 1,
      render_version: 0,
      state: 'pending',
      initiator_principal_id: 'feishu:tenant-1:ou-user-1'
    }),
    recordDelivery: async () => ({}),
    ...overrides
  } as unknown as FeishuSessionApi
}

function renderer(): FeishuRenderer {
  return {
    updateCard: async () => {}
  } as unknown as FeishuRenderer
}
