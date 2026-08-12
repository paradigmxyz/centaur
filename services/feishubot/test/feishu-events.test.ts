import { describe, expect, it } from 'bun:test'
import * as Lark from '@larksuiteoapi/node-sdk'
import {
  buildFeishuClientConfig,
  normalizeFeishuCardAction,
  normalizeFeishuMessage
} from '../src/feishu-events.js'

describe('China Feishu event normalization', () => {
  it('pins both official clients to China Feishu self-built apps', () => {
    const config = buildFeishuClientConfig('cli_a', 'secret')
    expect(config.domain).toBe(Lark.Domain.Feishu)
    expect(config.appType).toBe(Lark.AppType.SelfBuild)
  })

  it('binds direct messages to the sender and normalizes commands', () => {
    const normalized = normalizeFeishuMessage({
      schema: '2.0',
      header: { event_id: 'evt-1', tenant_key: 'tenant-1' },
      event: {
        sender: {
          sender_type: 'user',
          sender_id: { open_id: 'ou-user-1', union_id: 'on-user-1' }
        },
        message: {
          message_id: 'om-1',
          chat_id: 'oc-dm',
          chat_type: 'p2p',
          message_type: 'text',
          content: JSON.stringify({ text: '  /new  ' }),
          mentions: []
        }
      }
    }, { botOpenId: 'ou-bot' })

    expect(normalized).toMatchObject({
      eventId: 'evt-1',
      messageId: 'om-1',
      tenantKey: 'tenant-1',
      conversationKey: 'ou-user-1',
      rootMessageId: 'direct',
      principalId: 'feishu:tenant-1:ou-user-1',
      command: 'new',
      text: '/new',
      isDirect: true
    })
  })

  it('requires a group mention, removes only the bot mention, and keeps the topic root', () => {
    const base = {
      schema: '2.0',
      header: { event_id: 'evt-2', tenant_key: 'tenant-1' },
      event: {
        sender: { sender_type: 'user', sender_id: { open_id: 'ou-user-2' } },
        message: {
          message_id: 'om-2',
          root_id: 'om-root',
          chat_id: 'oc-group',
          chat_type: 'group',
          message_type: 'text',
          content: JSON.stringify({ text: '@_user_1 修复代码，并联系 @_user_2' }),
          mentions: [
            { key: '@_user_1', id: { open_id: 'ou-bot' }, name: 'Centaur' },
            { key: '@_user_2', id: { open_id: 'ou-human' }, name: '同事' }
          ]
        }
      }
    }
    expect(normalizeFeishuMessage(base, { botOpenId: 'ou-bot' })).toMatchObject({
      conversationKey: 'oc-group',
      rootMessageId: 'om-root',
      text: '修复代码，并联系 @_user_2',
      isDirect: false
    })
    expect(normalizeFeishuMessage({
      ...base,
      event: {
        ...base.event,
        message: { ...base.event.message, mentions: [] }
      }
    }, { botOpenId: 'ou-bot' })).toBeNull()
  })

  it('rejects bot/self events and malformed or oversized content', () => {
    const message = {
      schema: '2.0',
      header: { event_id: 'evt-3', tenant_key: 'tenant-1' },
      event: {
        sender: { sender_type: 'app', sender_id: { open_id: 'ou-bot' } },
        message: {
          message_id: 'om-3', chat_id: 'oc-dm', chat_type: 'p2p',
          message_type: 'text', content: JSON.stringify({ text: 'hello' }), mentions: []
        }
      }
    }
    expect(normalizeFeishuMessage(message, { botOpenId: 'ou-bot' })).toBeNull()
    expect(() => normalizeFeishuMessage({
      ...message,
      event: {
        ...message.event,
        sender: { sender_type: 'user', sender_id: { open_id: 'ou-user' } },
        message: {
          ...message.event.message,
          content: JSON.stringify({ text: 'x'.repeat(40_001) })
        }
      }
    }, { botOpenId: 'ou-bot' })).toThrow('too long')
  })

  it('normalizes current and legacy card-action operator identities', () => {
    expect(normalizeFeishuCardAction({
      event_id: 'evt-card-v2',
      tenant_key: 'tenant-1',
      operator: {
        operator_id: { open_id: 'ou-user-1', union_id: 'on-user-1' }
      },
      action: { value: { action: 'confirm', selection_flow_id: 'sel_1' } },
      context: { open_message_id: 'om-card-1' }
    })).toMatchObject({
      eventId: 'evt-card-v2',
      tenantKey: 'tenant-1',
      operatorOpenId: 'ou-user-1',
      operatorUnionId: 'on-user-1',
      messageId: 'om-card-1',
      action: 'confirm'
    })
    expect(normalizeFeishuCardAction({
      token: 'evt-card-legacy', tenant_key: 'tenant-1', open_id: 'ou-user-2',
      open_message_id: 'om-card-2', action: { value: { action: 'cancel' } }
    })).toMatchObject({ operatorOpenId: 'ou-user-2', action: 'cancel' })
  })
})
