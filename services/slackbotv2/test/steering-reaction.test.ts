import { describe, expect, test } from 'bun:test'
import type { Message as ChatMessage, Thread } from 'chat'
import { createSteeringReactionController } from '../src/steering-reaction'

describe('steering reaction acknowledgements', () => {
  test('absorbs Slack API failures', async () => {
    const controller = createSteeringReactionController({
      apiUrl: 'http://api.test',
      botToken: 'xoxb-test',
      fetch: async () => {
        throw new Error('Slack is unavailable')
      },
      signingSecret: 'test-secret',
      steeringReactionEnabled: true
    })
    const ack = controller.begin(
      { id: 'slack:C123:1700000000.000100' } as Thread,
      { id: '1700000001.000200' } as ChatMessage
    )

    expect(ack).toBeDefined()
    expect(await ack!.added).toBe(false)
    await expect(controller.complete(ack!)).resolves.toBeUndefined()
  })
})
