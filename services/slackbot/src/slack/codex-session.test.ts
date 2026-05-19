import { describe, expect, it } from 'bun:test'
import { composedAssistantStreamMarkdown } from './render'
import { AgentSessionRenderer } from './agent-session'
import { CodexSessionRenderer } from './codex-session'

describe('assistant message sections', () => {
  it('composes blockquoted commentary before the answer', () => {
    expect(composedAssistantStreamMarkdown('line one\nline two', 'Final answer.')).toBe(
      '> line one\n> line two\n\nFinal answer.'
    )
  })
})

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
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
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
      id: 'cmd-1',
      title: '1. Command execution',
      status: 'complete',
      output: 'one\ntwo\n'
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
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
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
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping', exitCode: 0 }
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'cmd-2',
        type: 'commandExecution',
        command: 'call grafana health',
        exitCode: 1
      }
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    expect(new Set(taskUpdates.map(chunk => chunk.id))).toEqual(new Set(['cmd-1', 'cmd-2']))
    expect(
      taskUpdates.some(
        chunk => chunk.id === 'cmd-1' && String(chunk.details ?? '').includes('call demo ping')
      )
    ).toBe(true)
    expect(taskUpdates.at(-1)).toMatchObject({
      id: 'cmd-2',
      status: 'complete',
      title: '2. Command execution'
    })
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
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
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

    expect(calls.some(call => call.method === 'chat.stopStream')).toBe(true)
    const update = calls.find(call => call.method === 'chat.update')
    expect(update?.params.blocks?.[0]?.type).toBe('plan')
    expect(update?.params.blocks?.[0]?.tasks?.[0]?.status).toBe('complete')
    expect(update?.params.blocks?.[0]?.tasks?.[0]?.title).toBe('1. Command execution')
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
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
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
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'call discover grafana',
        exitCode: 0
      }
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    const output = taskUpdates.map(chunk => richTextPlain(chunk.output)).join('\n')
    expect(output).toContain('"tool": "grafana"')
    expect(JSON.stringify(taskUpdates.map(chunk => chunk.output))).not.toContain('```text')
    expect(output).not.toContain('"method-11"')
  })

  it('unwraps bash wrappers in command details', async () => {
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
        command: "/bin/bash -lc 'call tools'"
      }
    })
    await renderer.event(sessionId, { type: 'turn.completed' })

    const update = calls.find(call => call.method === 'chat.update')
    const detailsText =
      update?.params.blocks?.[0]?.tasks?.[0]?.details?.elements?.[0]?.elements?.[0]?.text ?? ''
    expect(update?.params.blocks?.[0]?.tasks?.[0]?.title).toBe('1. Command execution')
    expect(detailsText).toBe('call tools')
    expect(detailsText).not.toContain('/bin/bash')
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
        },
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
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
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'call tools',
        exitCode: 0
      }
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')

    const output = taskUpdates.map(chunk => richTextPlain(chunk.output)).join('\n')
    expect(output).toContain('"demo"')
    expect(JSON.stringify(taskUpdates.map(chunk => chunk.output))).not.toContain('```text')
    expect(output).not.toContain('"grafana"')
  })

  it('keeps neutral task titles and formats tool errors in output', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: {
        threads: {
          setStatus: async () => ({ ok: true })
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
        stopStream: async () => ({ ok: true }),
        update: async () => ({ ok: true })
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
      type: 'assistant',
      message: {
        content: [
          {
            type: 'tool_use',
            id: 'tool-1',
            name: 'websearch',
            input: { query: 'centaur slackbot' }
          }
        ]
      }
    })
    await renderer.event(sessionId, {
      type: 'user',
      content: [
        {
          type: 'tool_result',
          tool_use_id: 'tool-1',
          is_error: true,
          content: JSON.stringify({ error: 'rate limited', status: 429 })
        }
      ]
    })

    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'task_update')
    const last = taskUpdates.at(-1)

    expect(last?.title).toBe('Use websearch')
    expect(last?.status).toBe('complete')
    expect(richTextPlain(last?.output)).toContain('"error": "rate limited"')
    expect(last?.title).not.toContain('failed')
  })

  it('inserts a blank line between consecutive commentary agent messages', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: {
        threads: {
          setStatus: async () => ({ ok: true })
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
        stopStream: async () => ({ ok: true }),
        update: async () => ({ ok: true })
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
      item: { id: 'msg-1', type: 'agentMessage', phase: 'commentary' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: 'First commentary paragraph.'
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'msg-1',
        type: 'agentMessage',
        phase: 'commentary',
        text: 'First commentary paragraph.'
      }
    })
    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'msg-2', type: 'agentMessage', phase: 'commentary' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-2',
      delta: 'Second commentary paragraph.'
    })
    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const streamed = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')

    expect(streamed).toBe(
      '> First commentary paragraph.\n> \n> Second commentary paragraph.'
    )
  })

  it('streams commentary as blockquote and finalizes with context + markdown blocks', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: {
        threads: {
          setStatus: async () => ({ ok: true })
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
        stopStream: async () => ({ ok: true }),
        update: async (params: any) => {
          calls.push({ method: 'chat.update', params })
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
      item: { id: 'msg-1', type: 'agentMessage', phase: 'commentary' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: 'Planning the tool calls.'
    })
    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'msg-2', type: 'agentMessage', phase: 'final_answer' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-2',
      delta: 'Done: five tools called.'
    })
    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done: five tools called.' })

    const streamed = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')
    expect(streamed).toContain('> Planning the tool calls.')
    expect(streamed).toContain('Done: five tools called.')

    const update = calls.find(call => call.method === 'chat.update')
    const blocks = update?.params.blocks ?? []
    expect(blocks.some((block: any) => block.type === 'context')).toBe(true)
    expect(
      blocks.some(
        (block: any) =>
          block.type === 'markdown' && String(block.text).includes('Done: five tools called.')
      )
    ).toBe(true)
    expect(
      blocks.some(
        (block: any) =>
          block.type === 'context' &&
          String(block.elements?.[0]?.text ?? '').includes('Planning the tool calls.')
      )
    ).toBe(true)
  })
})

function richTextPlain(value: any): string {
  if (!value) return ''
  if (typeof value === 'string') return value
  return (value.elements ?? [])
    .map((element: any) =>
      (element.elements ?? [])
        .map((inline: any) => inline.text ?? inline.url ?? inline.user_id ?? '')
        .join('')
    )
    .join('\n')
}
