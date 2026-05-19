import { describe, expect, it } from 'bun:test'
import { AgentSessionRenderer } from './agent-session'

describe('AgentSessionRenderer', () => {
  it('streams pending text before appending inline task updates', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
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
          return { ok: true, ts: '1778866940.295499' }
        },
        appendStream: async (params: any) => {
          calls.push({ method: 'chat.appendStream', params })
          return { ok: true }
        },
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
          return { ok: true }
        }
      }
    }

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    await renderer.text(sessionId, '```python\nprint("Hello, world!")\n```\n\nTiny keys wake up\n')
    await renderer.step(sessionId, {
      id: 'sleep-1',
      title: 'Run command',
      status: 'in_progress',
      details: '```bash\nsleep 2\n```'
    })
    await renderer.text(sessionId, '\n```js\nconsole.log("Hello, world!")\n```')
    await renderer.done(sessionId)

    const start = calls.find(call => call.method === 'chat.startStream')
    expect(start?.params.task_display_mode).toBe('plan')
    expect(start?.params.chunks).toEqual([
      { type: 'plan_update', title: 'Centaur execution' },
      {
        type: 'task_update',
        id: 'sleep-1',
        title: 'Run command',
        status: 'in_progress',
        details: '```bash\nsleep 2\n```'
      }
    ])
    expect(calls.slice(0, 3).map(call => call.method)).toEqual([
      'assistant.threads.setStatus',
      'chat.startStream',
      'assistant.threads.setStatus'
    ])
    expect(calls[0]?.params.status).toBe('Thinking...')
    expect(calls[0]?.params.loading_messages).toEqual(['Thinking...'])
    expect(calls[2]?.params.status).toBe('')
    expect(calls[2]?.params.loading_messages).toBeUndefined()

    const appends = calls.filter(call => call.method === 'chat.appendStream')
    expect(appends[0]?.params.chunks).toEqual([
      {
        type: 'markdown_text',
        text: '```python\nprint("Hello, world!")\n```\n\nTiny keys wake up\n'
      }
    ])
    expect(appends[1]?.params.chunks).toEqual([
      { type: 'markdown_text', text: '\n```js\nconsole.log("Hello, world!")\n```' }
    ])
    const update = calls.find(call => call.method === 'chat.update')
    expect(update?.params.blocks?.[0]?.type).toBe('plan')
    expect(update?.params.blocks?.[0]?.tasks?.[0]?.status).toBe('complete')
  })

  it('streams task updates with accumulated details and output', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
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
          return { ok: true, ts: '1778866940.295499' }
        },
        appendStream: async (params: any) => {
          calls.push({ method: 'chat.appendStream', params })
          return { ok: true }
        },
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
          return { ok: true }
        }
      }
    }

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    await renderer.step(sessionId, {
      id: 'cmd-1',
      title: 'Run command: pnpm test',
      status: 'in_progress',
      details: '```bash\npnpm test\n```'
    })
    await renderer.step(sessionId, {
      id: 'cmd-1',
      title: 'Run command: pnpm test',
      status: 'complete',
      output: '```text\nok\n```'
    })

    const start = calls.find(call => call.method === 'chat.startStream')
    expect(start?.params.task_display_mode).toBe('plan')
    expect(start?.params.chunks?.[0]).toEqual({
      type: 'plan_update',
      title: 'Centaur execution'
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    expect(taskUpdates.at(-1)).toEqual({
      type: 'task_update',
      id: 'cmd-1',
      title: 'Run command: pnpm test',
      status: 'complete',
      output: '```text\nok\n```'
    })
  })

  it('clears assistant status even when closing the stream fails', async () => {
    const calls: Array<{ method: string; params: any }> = []
    let stopAttempts = 0
    const client = {
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
          return { ok: true, ts: '1778866940.295499' }
        },
        appendStream: async (params: any) => {
          calls.push({ method: 'chat.appendStream', params })
          return { ok: true }
        },
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          stopAttempts += 1
          if (stopAttempts === 2) return { ok: true }
          return { ok: false, error: 'stream_already_closed' }
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
          return { ok: true }
        }
      }
    }

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    await renderer.text(sessionId, 'Finished reply')
    await expect(renderer.done(sessionId)).rejects.toThrow('stream_already_closed')

    expect(calls.at(-1)).toEqual({
      method: 'assistant.threads.setStatus',
      params: {
        channel_id: 'C123',
        thread_ts: '1778866921.505479',
        status: ''
      }
    })

    await expect(renderer.done(sessionId)).resolves.toBeUndefined()
    expect(stopAttempts).toBe(2)
  })
})
