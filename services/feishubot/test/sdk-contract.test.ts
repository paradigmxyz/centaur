import { describe, expect, it } from 'bun:test'
import * as Lark from '@larksuiteoapi/node-sdk'

describe('China Feishu SDK event boundary', () => {
  it('dispatches message and card callbacks through the long-connection dispatcher', async () => {
    const received: string[] = []
    const dispatcher = new Lark.EventDispatcher({})
    dispatcher.register({
      'im.message.receive_v1': () => {
        received.push('message')
      },
      'card.action.trigger': () => {
        received.push('card')
      }
    })

    await dispatcher.invoke({
      schema: '2.0',
      header: {
        event_id: 'evt-message',
        event_type: 'im.message.receive_v1',
        tenant_key: 'tenant-1'
      },
      event: {
        sender: { sender_type: 'user', sender_id: { open_id: 'ou-user' } },
        message: {
          message_id: 'om-message', chat_id: 'oc-chat', chat_type: 'p2p',
          message_type: 'text', content: JSON.stringify({ text: 'hello' })
        }
      }
    }, { needCheck: false })
    await dispatcher.invoke({
      schema: '2.0',
      header: {
        event_id: 'evt-card',
        event_type: 'card.action.trigger',
        tenant_key: 'tenant-1'
      },
      event: {
        operator: { operator_id: { open_id: 'ou-user' } },
        action: { value: { action: 'confirm' } },
        context: { open_message_id: 'om-card' }
      }
    }, { needCheck: false })

    expect(received).toEqual(['message', 'card'])
  })
})
