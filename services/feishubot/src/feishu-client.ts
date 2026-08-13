import * as Lark from '@larksuiteoapi/node-sdk'
import type { RenderedCard } from './selection-cards.js'
import { FeishuMetrics } from './metrics.js'

export class FeishuRenderer {
  constructor(
    private readonly client: Lark.Client,
    private readonly metrics: FeishuMetrics = new FeishuMetrics()
  ) {}

  async replyCard(
    messageId: string,
    rendered: RenderedCard,
    inThread: boolean,
    idempotencyKey: string = crypto.randomUUID()
  ): Promise<string> {
    return this.observe('reply_card', async () => {
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
    })
  }

  async replyText(messageId: string, text: string, inThread: boolean): Promise<string> {
    return this.observe('reply_text', async () => {
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
    })
  }

  async updateCard(messageId: string, rendered: RenderedCard): Promise<void> {
    return this.observe('update_card', async () => {
      const result = await this.client.im.v1.message.patch({
        path: { message_id: messageId },
        data: { content: JSON.stringify(rendered.card) }
      })
      assertFeishuResult(result, 'update card')
    })
  }

  private async observe<T>(
    operation: 'reply_card' | 'reply_text' | 'update_card',
    callback: () => Promise<T>
  ): Promise<T> {
    const startedAt = performance.now()
    try {
      const result = await callback()
      this.metrics.recordRender(operation, 'succeeded', performance.now() - startedAt)
      return result
    } catch (error) {
      this.metrics.recordRender(operation, 'failed', performance.now() - startedAt)
      throw error
    }
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
