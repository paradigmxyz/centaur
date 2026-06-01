import { describe, expect, it } from 'bun:test'
import { SlackChatSDKRenderer, SlackStreamRenderer } from './slack'

describe('Slack rendering adapters', () => {
  it('drives legacy agent sessions from renderer events', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const renderer = new SlackChatSDKRenderer(slackClient(calls) as any)

    const opened = await renderer.open({
      title: 'Centaur execution',
      target: {
        channel: 'C123',
        parentTs: '1778866921.505479',
        recipientTeamId: 'T123',
        recipientUserId: 'U123'
      }
    })

    expect(opened.sessionId).toBeTruthy()

    await renderer.render(opened.sessionId!, {
      type: 'renderer.message.delta',
      delta: 'Working on it.'
    })
    await renderer.render(opened.sessionId!, {
      type: 'renderer.task.update',
      task: {
        id: 'task-1',
        title: 'Run command',
        status: 'in_progress',
        details: [{ type: 'text', text: '```bash\npnpm test\n```' }]
      }
    })
    const closed = await renderer.close(opened.sessionId!, { type: 'renderer.done' })

    const streamed = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])

    expect(streamed).toContainEqual({ type: 'markdown_text', text: 'Working on it.' })
    expect(streamed).toContainEqual({
      type: 'task_update',
      id: 'task-1',
      title: 'Run command',
      status: 'in_progress',
      details: '```bash\npnpm test\n```'
    })
    expect(closed.closed).toBe(true)
    expect(calls.some(call => call.method === 'chat.stopStream')).toBe(true)
  })

  it('maps legacy stream markdown through Chat SDK-shaped stream chunks', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const renderer = new SlackStreamRenderer(slackClient(calls) as any)

    const started = await renderer.start({
      target: {
        channel: 'C123',
        threadTs: '1778866921.505479',
        recipientTeamId: 'T123',
        recipientUserId: 'U123'
      },
      markdown: 'Hello'
    })
    await renderer.append({
      target: { channel: 'C123', ts: started.ts! },
      chunks: [{ type: 'markdown_text', text: ' world' }]
    })
    await renderer.stop({
      target: { channel: 'C123', ts: started.ts! },
      markdown: 'Done.'
    })

    expect(calls.find(call => call.method === 'chat.startStream')?.params.chunks).toEqual([
      { type: 'markdown_text', text: 'Hello' }
    ])
    expect(calls.find(call => call.method === 'chat.appendStream')?.params.chunks).toEqual([
      { type: 'markdown_text', text: ' world' }
    ])
    expect(calls.find(call => call.method === 'chat.stopStream')?.params.chunks).toEqual([
      { type: 'markdown_text', text: 'Done.' }
    ])
  })
})

function slackClient(calls: Array<{ method: string; params: any }>) {
  return {
    assistant: {
      threads: {
        setStatus: async (params: any) => {
          calls.push({ method: 'assistant.threads.setStatus', params })
          return { ok: true }
        }
      }
    },
    chat: {
      startStream: async (params: any) => {
        calls.push({ method: 'chat.startStream', params })
        return { ok: true, channel: params.channel, ts: '1778866940.295499' }
      },
      appendStream: async (params: any) => {
        calls.push({ method: 'chat.appendStream', params })
        return { ok: true, channel: params.channel, ts: params.ts }
      },
      stopStream: async (params: any) => {
        calls.push({ method: 'chat.stopStream', params })
        return { ok: true, channel: params.channel, ts: params.ts }
      },
      update: async (params: any) => {
        calls.push({ method: 'chat.update', params })
        return { ok: true }
      }
    }
  }
}
