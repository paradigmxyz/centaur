import { describe, expect, it } from 'bun:test'
import { FeishuBot } from '../src/bot.js'
import type { FeishuRenderer } from '../src/feishu-client.js'
import { FeishuApiError, type FeishuDelivery, type FeishuSessionApi } from '../src/session-api.js'

describe('Feishu per-message delivery', () => {
  it('replies to the current source message with a generation-scoped idempotency key', async () => {
    const replies: Array<{
      sourceMessageId: string
      inThread: boolean
      idempotencyKey: string
    }> = []
    const recordedGenerations: Array<number | undefined> = []
    const recordedOwners: Array<string | undefined> = []
    const renderCompletions: Array<boolean | undefined> = []
    let claimCount = 0
    let delivery = executionDelivery({
      source_message_id: 'om-user-2',
      message_id: null,
      delivery_generation: 2
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getDelivery: async () => delivery,
        claimDelivery: async () => {
          claimCount += 1
          return delivery
        },
        recordDelivery: async (...args: unknown[]) => {
          recordedGenerations.push(args[4] as number | undefined)
          recordedOwners.push(args[5] as string | undefined)
          renderCompletions.push(args[6] as boolean | undefined)
          delivery = {
            ...delivery,
            message_id: args[1] as string,
            last_event_cursor: args[2] as number,
            render_version: delivery.desired_version,
            state: 'delivered'
          }
          return delivery
        },
        streamEvents: async function * () {
          yield { id: 42, event: 'development.changeset_empty', data: {} }
        }
      } as unknown as FeishuSessionApi,
      renderer: {
        replyCard: async (
          sourceMessageId: Parameters<FeishuRenderer['replyCard']>[0],
          _card: Parameters<FeishuRenderer['replyCard']>[1],
          inThread: Parameters<FeishuRenderer['replyCard']>[2],
          idempotencyKey: Parameters<FeishuRenderer['replyCard']>[3]
        ) => {
          if (!idempotencyKey) throw new Error('expected a delivery idempotency key')
          replies.push({ sourceMessageId, inThread, idempotencyKey })
          return 'om-card-2'
        },
        updateCard: async () => {}
      } as unknown as FeishuRenderer
    })

    await bot.reconcileDelivery('development:1')
    await Bun.sleep(10)

    expect(replies).toEqual([{
      sourceMessageId: 'om-user-2',
      inThread: false,
      idempotencyKey: 'fdl_1-2'
    }])
    expect(recordedGenerations).toEqual([2, 2])
    expect(claimCount).toBe(3)
    expect(renderCompletions).toEqual([false, true])
    expect(recordedOwners[0]).toBeTruthy()
    expect(new Set(recordedOwners).size).toBe(1)
  })

  it('passes the triggering message identity when opening projects', async () => {
    const addSelectionCalls: Array<{
      threadKey: string
      principalId: string
      sourceMessageId: string | undefined
      idempotencyKey: string | undefined
    }> = []
    let markRendered: () => void = () => {}
    const rendered = new Promise<void>(resolve => {
      markRendered = resolve
    })
    const delivery = executionDelivery({
      source_message_id: 'om-projects',
      message_id: null,
      execution_id: null,
      selection_flow_id: 'sel_add',
      delivery_generation: 3
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        activeBinding: async () => ({ thread_key: 'development:1', workspace_id: 'wsp_1' }),
        createAddSelection: async (...args: string[]) => {
          addSelectionCalls.push({
            threadKey: args[0]!,
            principalId: args[1]!,
            sourceMessageId: args[2],
            idempotencyKey: args[3]
          })
          return { selection_flow_id: 'sel_add', workspace_id: 'wsp_1', version: 1 }
        },
        getSelection: async () => ({
          selection_flow_id: 'sel_add',
          workspace_id: 'wsp_1',
          thread_key: 'development:1',
          execution_id: null,
          kind: 'add',
          state: 'pending',
          version: 1,
          task_excerpt: '',
          query: '',
          cursor: null,
          cursor_history: [],
          selected_repository_ids: []
        }),
        searchRepositories: async () => ({ repositories: [], next_cursor: null }),
        getDelivery: async () => delivery,
        claimDelivery: async () => delivery,
        recordDelivery: async () => delivery
      } as unknown as FeishuSessionApi,
      renderer: {
        replyCard: async () => {
          markRendered()
          return 'om-projects-card'
        }
      } as unknown as FeishuRenderer
    })

    bot.acceptMessageEvent(messageEvent('/projects', 'om-projects'))
    await rendered

    expect(addSelectionCalls).toEqual([{
      threadKey: 'development:1',
      principalId: 'feishu:tenant-1:ou-user-1',
      sourceMessageId: 'om-projects',
      idempotencyKey: 'evt-om-projects'
    }])
  })

  it('replies below a concurrent projects command when the prior card is not recorded yet', async () => {
    const replies: Array<{ messageId: string; text: string }> = []
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        activeBinding: async () => ({ thread_key: 'development:1', workspace_id: 'wsp_1' }),
        createAddSelection: async () => {
          throw new FeishuApiError('add projects', 409, false)
        }
      } as unknown as FeishuSessionApi,
      renderer: {
        replyText: async (messageId: string, reply: string) => {
          replies.push({ messageId, text: reply })
          return 'om-retry-projects'
        }
      } as unknown as FeishuRenderer
    })

    bot.acceptMessageEvent(messageEvent('/projects', 'om-projects-2'))
    await Bun.sleep(20)

    expect(replies).toHaveLength(1)
    expect(replies[0]?.messageId).toBe('om-projects-2')
    expect(replies[0]?.text).toContain('稍后重试')
  })

  it('shares one execution stream across concurrent reconciliation', async () => {
    let streamCount = 0
    let releaseStream: () => void = () => {}
    const released = new Promise<void>(resolve => {
      releaseStream = resolve
    })
    const delivery = executionDelivery({ message_id: 'om-card-2', delivery_generation: 2 })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getDelivery: async () => delivery,
        claimDelivery: async () => delivery,
        recordDelivery: async () => delivery,
        streamEvents: async function * () {
          streamCount += 1
          await released
          yield { id: 42, event: 'development.changeset_empty', data: {} }
        }
      } as unknown as FeishuSessionApi,
      renderer: { updateCard: async () => {} } as unknown as FeishuRenderer
    })

    const first = bot.reconcileDelivery('development:1')
    const second = bot.reconcileDelivery('development:1')
    await Bun.sleep(10)
    const observedStreams = streamCount
    releaseStream()
    await Promise.all([first, second])
    await Bun.sleep(10)

    expect(observedStreams).toBe(1)
  })

  it('ignores a delayed duplicate after a newer delivery generation exists', async () => {
    let claimCount = 0
    let renderCount = 0
    const current = executionDelivery({
      source_message_id: 'om-user-2',
      message_id: 'om-card-2',
      delivery_generation: 2,
      execution_id: 'exe_2'
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        acceptMessage: async () => ({
          thread_key: current.thread_key,
          workspace_id: 'wsp_1',
          execution_id: 'exe_1',
          created: false
        }),
        getDelivery: async () => current,
        claimDelivery: async () => {
          claimCount += 1
          return current
        }
      } as unknown as FeishuSessionApi,
      renderer: {
        updateCard: async () => { renderCount += 1 },
        replyCard: async () => {
          renderCount += 1
          return 'om-unexpected'
        }
      } as unknown as FeishuRenderer
    })

    bot.acceptMessageEvent(messageEvent('old request', 'om-user-1'))
    await Bun.sleep(20)

    expect(claimCount).toBe(0)
    expect(renderCount).toBe(0)
  })

  it('replays execution history from zero before rendering events after the durable cursor', async () => {
    let requestedAfterEventId: number | undefined
    let renderedCard = ''
    const delivery = executionDelivery({
      message_id: 'om-card-1',
      last_event_cursor: 2
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getDelivery: async () => delivery,
        claimDelivery: async () => delivery,
        recordDelivery: async () => delivery,
        streamEvents: async function * (_threadKey: string, _executionId: string, afterEventId: number) {
          requestedAfterEventId = afterEventId
          if (afterEventId < 1) {
            yield {
              id: 1,
              event: 'session.output.line',
              data: JSON.stringify({
                type: 'item.started',
                item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
              })
            }
          }
          if (afterEventId < 2) {
            yield {
              id: 2,
              event: 'session.output.line',
              data: JSON.stringify({
                type: 'item.agentMessage.delta',
                itemId: 'msg-1',
                delta: '历史回答'
              })
            }
          }
          yield { id: 3, event: 'development.changeset_empty', data: {} }
        }
      } as unknown as FeishuSessionApi,
      renderer: {
        updateCard: async (_messageId: string, card: Parameters<FeishuRenderer['updateCard']>[1]) => {
          renderedCard = JSON.stringify(card.card)
        }
      } as unknown as FeishuRenderer
    })

    await bot.reconcileDelivery(delivery.thread_key)

    expect(requestedAfterEventId).toBe(0)
    expect(renderedCard).toContain('历史回答')
  })

  it('ignores a selection action from a card that no longer owns the delivery generation', async () => {
    let mutationCount = 0
    let renderCount = 0
    const current = executionDelivery({
      source_message_id: 'om-projects-2',
      message_id: 'om-card-2',
      delivery_generation: 2,
      execution_id: null,
      selection_flow_id: 'sel_add'
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getSelection: async () => ({
          selection_flow_id: 'sel_add',
          workspace_id: 'wsp_1',
          thread_key: current.thread_key,
          execution_id: null,
          kind: 'add',
          state: 'pending',
          version: 1,
          task_excerpt: '',
          query: '',
          cursor: null,
          cursor_history: [],
          selected_repository_ids: []
        }),
        getDelivery: async () => current,
        confirmNoProject: async () => { mutationCount += 1 }
      } as unknown as FeishuSessionApi,
      renderer: {
        updateCard: async () => { renderCount += 1 }
      } as unknown as FeishuRenderer
    })

    bot.acceptCardEvent(selectionCardEvent('om-card-1', 'sel_add', 'no_project'))
    await Bun.sleep(20)

    expect(mutationCount).toBe(0)
    expect(renderCount).toBe(0)
  })

  it('does not visibly update a selection card without owning the render lease', async () => {
    let renderCount = 0
    const current = executionDelivery({
      source_message_id: 'om-projects',
      message_id: 'om-card-1',
      execution_id: null,
      selection_flow_id: 'sel_add'
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getSelection: async () => ({
          selection_flow_id: 'sel_add',
          workspace_id: 'wsp_1',
          thread_key: current.thread_key,
          execution_id: null,
          kind: 'add',
          state: 'pending',
          version: 1,
          task_excerpt: '',
          query: '',
          cursor: null,
          cursor_history: [],
          selected_repository_ids: []
        }),
        getDelivery: async () => current,
        confirmNoProject: async () => ({}),
        claimDelivery: async () => {
          throw new FeishuApiError('claim delivery', 409, false)
        }
      } as unknown as FeishuSessionApi,
      renderer: {
        updateCard: async () => { renderCount += 1 }
      } as unknown as FeishuRenderer
    })

    bot.acceptCardEvent(selectionCardEvent('om-card-1', 'sel_add', 'no_project'))
    await Bun.sleep(20)

    expect(renderCount).toBe(0)
  })

  it('ignores a publication action from a card that no longer owns the delivery generation', async () => {
    let approvalCount = 0
    let renderCount = 0
    const current = executionDelivery({
      source_message_id: 'om-user-2',
      message_id: 'om-card-2',
      delivery_generation: 2,
      execution_id: 'exe_2'
    })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getChangeset: async () => ({ thread_key: current.thread_key }),
        getDelivery: async () => current,
        approvePublication: async () => {
          approvalCount += 1
          return {}
        }
      } as unknown as FeishuSessionApi,
      renderer: {
        updateCard: async () => { renderCount += 1 }
      } as unknown as FeishuRenderer
    })

    bot.acceptCardEvent(publicationCardEvent('om-card-1', 'approve_publication', {
      changeset_id: 'chg_old'
    }))
    await Bun.sleep(20)

    expect(approvalCount).toBe(0)
    expect(renderCount).toBe(0)
  })

  it('recovers a cancelled selection as a terminal card instead of opening an execution stream', async () => {
    let renderedCard = ''
    const delivery = executionDelivery({ selection_flow_id: 'sel_1' })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getDelivery: async () => delivery,
        getSelection: async () => ({
          selection_flow_id: 'sel_1',
          workspace_id: 'wsp_1',
          thread_key: delivery.thread_key,
          execution_id: delivery.execution_id,
          kind: 'initial',
          state: 'cancelled',
          version: 2,
          task_excerpt: 'cancelled task',
          query: '',
          cursor: null,
          cursor_history: [],
          selected_repository_ids: []
        }),
        claimDelivery: async () => delivery,
        recordDelivery: async () => delivery
      } as unknown as FeishuSessionApi,
      renderer: {
        updateCard: async (_messageId: string, card: Parameters<FeishuRenderer['updateCard']>[1]) => {
          renderedCard = JSON.stringify(card.card)
        }
      } as unknown as FeishuRenderer
    })

    await bot.reconcileDelivery(delivery.thread_key)

    expect(renderedCard).toContain('任务已取消')
  })

  it('does not open an execution event stream without owning the render lease', async () => {
    let streamCount = 0
    const delivery = executionDelivery()
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getDelivery: async () => delivery,
        claimDelivery: async () => {
          throw new FeishuApiError('claim delivery', 409, false)
        },
        streamEvents: async function * () {
          streamCount += 1
          yield { id: 1, event: 'development.changeset_empty', data: {} }
        }
      } as unknown as FeishuSessionApi,
      renderer: { updateCard: async () => {} } as unknown as FeishuRenderer
    })

    await bot.reconcileDelivery(delivery.thread_key)

    expect(streamCount).toBe(0)
  })

  it('shares one publication poller across concurrent reconciliation', async () => {
    let batchReads = 0
    const delivery = executionDelivery({ publish_batch_id: 'pub_1' })
    const bot = new FeishuBot({
      botOpenId: 'ou-bot',
      tenantAllowlist: new Set(['tenant-1']),
      api: {
        getDelivery: async () => delivery,
        claimDelivery: async () => delivery,
        recordDelivery: async () => delivery,
        getPublishBatch: async () => {
          batchReads += 1
          return {
            publish_batch_id: 'pub_1',
            changeset_id: 'chg_1',
            state: batchReads <= 2 ? 'running' : 'succeeded',
            items: []
          }
        }
      } as unknown as FeishuSessionApi,
      renderer: { updateCard: async () => {} } as unknown as FeishuRenderer
    })

    const first = bot.reconcileDelivery(delivery.thread_key)
    const second = bot.reconcileDelivery(delivery.thread_key)
    await Promise.all([first, second])

    expect(batchReads).toBe(3)
  })
})

function executionDelivery(overrides: Partial<FeishuDelivery> = {}): FeishuDelivery {
  return {
    delivery_id: 'fdl_1',
    tenant_key: 'tenant-1',
    thread_key: 'development:1',
    chat_id: 'oc-1',
    root_message_id: 'direct',
    source_message_id: 'om-user-1',
    message_id: 'om-card-1',
    last_event_cursor: 0,
    delivery_generation: 1,
    desired_version: 1,
    render_version: 0,
    state: 'pending',
    initiator_principal_id: 'feishu:tenant-1:ou-user-1',
    execution_id: 'exe_1',
    selection_flow_id: null,
    publish_batch_id: null,
    ...overrides
  }
}

function messageEvent(text: string, messageId: string): unknown {
  return {
    schema: '2.0',
    header: { event_id: `evt-${messageId}`, tenant_key: 'tenant-1' },
    event: {
      sender: {
        sender_type: 'user',
        sender_id: { open_id: 'ou-user-1' }
      },
      message: {
        message_id: messageId,
        chat_id: 'oc-dm',
        chat_type: 'p2p',
        message_type: 'text',
        content: JSON.stringify({ text }),
        mentions: []
      }
    }
  }
}

function selectionCardEvent(messageId: string, selectionFlowId: string, action: string): unknown {
  return {
    event_id: `evt-${messageId}-${action}`,
    tenant_key: 'tenant-1',
    operator: { operator_id: { open_id: 'ou-user-1' } },
    action: {
      value: {
        action,
        selection_flow_id: selectionFlowId,
        expected_version: 1
      }
    },
    context: { open_message_id: messageId }
  }
}

function publicationCardEvent(
  messageId: string,
  action: string,
  value: Record<string, unknown>
): unknown {
  return {
    event_id: `evt-${messageId}-${action}`,
    tenant_key: 'tenant-1',
    operator: { operator_id: { open_id: 'ou-user-1' } },
    action: { value: { action, ...value } },
    context: { open_message_id: messageId }
  }
}
