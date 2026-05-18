import { describe, expect, it } from 'bun:test'
import { AgentSessionRenderer } from './agent-session'
import { CodexSessionRenderer } from './codex-session'

describe('CodexSessionRenderer', () => {
  it('accumulates command output deltas into the same task update', async () => {
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
        }
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'pnpm --filter slackbot test'
      }
    })
    await renderer.event(sessionId, {
      type: 'item.commandExecution.outputDelta',
      itemId: 'cmd-1',
      delta: 'one\n'
    })
    await renderer.event(sessionId, {
      type: 'item.commandExecution.outputDelta',
      itemId: 'cmd-1',
      delta: 'two\n'
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'pnpm --filter slackbot test',
        exitCode: 0
      }
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    expect(taskUpdates.at(-1)).toEqual({
      type: 'task_update',
      id: 'codex-activity',
      title: 'Execution timeline',
      status: 'complete',
      details: undefined,
      output: '```text\n[done] Run command: pnpm --filter slackbot test\n```',
      sources: undefined
    })
  })

  it('renders multiple command executions as one visible activity task', async () => {
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
        }
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
    })
    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-2', type: 'commandExecution', command: 'call grafana health' }
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    expect(new Set(taskUpdates.map(chunk => chunk.id))).toEqual(new Set(['codex-activity']))
    expect(taskUpdates.at(-1)?.details).toBeUndefined()
    expect(taskUpdates.some(chunk => String(chunk.output).includes('call demo ping'))).toBe(true)
    expect(taskUpdates.at(-1)?.output).toContain('call grafana health')
    expect(taskUpdates.at(-1)?.output).toMatch(/^```text\n/)
  })

  it('marks the aggregate activity task complete on terminal turn events', async () => {
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
        }
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
    })
    await renderer.event(sessionId, { type: 'turn.completed' })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    expect(taskUpdates.at(-1)).toMatchObject({
      id: 'codex-activity',
      status: 'complete',
      title: 'Execution timeline',
      output: undefined
    })
    expect(calls.some(call => call.method === 'chat.stopStream')).toBe(true)
  })

  it('pretty prints JSON command output before streaming it', async () => {
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
        }
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'call discover grafana'
      }
    })
    await renderer.event(sessionId, {
      type: 'item.commandExecution.outputDelta',
      itemId: 'cmd-1',
      delta: JSON.stringify({
        tool: 'grafana',
        description: 'Grafana observability',
        methods: Array.from({ length: 12 }, (_, index) => ({
          name: `method-${index}`,
          description: `Run method ${index}`
        }))
      })
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    const output = taskUpdates.map(chunk => chunk.output ?? '').join('\n')
    expect(output).toContain('```text\n[run] Run command: call discover grafana')
    expect(output).not.toContain('```json')
    expect(output).not.toContain('"method-11"')
  })

  it('previews tool list output before streaming it', async () => {
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
        }
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'call tools'
      }
    })
    await renderer.event(sessionId, {
      type: 'item.commandExecution.outputDelta',
      itemId: 'cmd-1',
      delta: JSON.stringify({
        demo: {
          description: 'Demo tool',
          methods: Array.from({ length: 20 }, (_, index) => `method-${index}`)
        },
        grafana: {
          description: 'Grafana observability',
          methods: ['health', 'query']
        }
      })
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    const output = taskUpdates.map(chunk => chunk.output ?? '').join('\n')
    expect(output).toContain('```text\n[run] Run command: call tools')
    expect(output).not.toContain('```json')
    expect(output).not.toContain('"grafana"')
  })
})
