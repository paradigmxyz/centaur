import * as Lark from '@larksuiteoapi/node-sdk'
import type { RenderedCard } from './selection-cards.js'

export class FeishuRenderer {
  constructor(private readonly client: Lark.Client) {}

  async replyCard(
    messageId: string,
    rendered: RenderedCard,
    inThread: boolean,
    idempotencyKey: string = crypto.randomUUID()
  ): Promise<string> {
    const result = await this.client.im.v1.message.reply({
      path: { message_id: messageId },
      data: {
        msg_type: 'interactive',
        content: JSON.stringify(rendered.card),
        reply_in_thread: inThread,
        uuid: idempotencyKey
      }
    })
    assertFeishuResult(result, 'reply card')
    const sent = result.data?.message_id
    if (!sent) throw new Error('Feishu reply card returned no message ID')
    return sent
  }

  async replyText(messageId: string, text: string, inThread: boolean): Promise<string> {
    const result = await this.client.im.v1.message.reply({
      path: { message_id: messageId },
      data: {
        msg_type: 'text',
        content: JSON.stringify({ text: bounded(text, 4_000) }),
        reply_in_thread: inThread,
        uuid: crypto.randomUUID()
      }
    })
    assertFeishuResult(result, 'reply text')
    const sent = result.data?.message_id
    if (!sent) throw new Error('Feishu reply text returned no message ID')
    return sent
  }

  async updateCard(messageId: string, rendered: RenderedCard): Promise<void> {
    const result = await this.client.im.v1.message.patch({
      path: { message_id: messageId },
      data: { content: JSON.stringify(rendered.card) }
    })
    assertFeishuResult(result, 'update card')
  }
}

function assertFeishuResult(result: { code?: number; msg?: string }, action: string): void {
  if (result.code && result.code !== 0) {
    throw new Error(`Feishu ${action} failed with code ${result.code}`)
  }
}

function bounded(value: string, max: number): string {
  return [...value].slice(0, max).join('')
}
