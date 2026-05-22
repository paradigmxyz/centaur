import { describe, expect, it } from 'bun:test'
import { shouldShowThinkingBlock } from './render'
import { AgentSessionRenderer } from './agent-session'
import { CodexSessionRenderer } from './codex-session'

describe('assistant message sections', () => {
  it('skips Thinking when commentary is duplicated in the answer', () => {
    const prose = 'I will call five tools and report results.'
    expect(shouldShowThinkingBlock(prose, prose)).toBe(false)
    expect(shouldShowThinkingBlock(prose, `${prose}\n\nTool results follow.`)).toBe(false)
    expect(shouldShowThinkingBlock('Planning only.', 'Final answer only.')).toBe(true)
  })
})

describe('CodexSessionRenderer', () => {
  it('streams canonical terminal result text before closing the session', async () => {
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

    const result = await renderer.event(sessionId, {
      type: 'result',
      text: 'PONG'
    })

    const streamed = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')
    expect(streamed).toContain('PONG')
    expect(result.done).toBe(true)
    expect(result.streamedAnswerChars).toBe(4)
    expect(calls.some(call => call.method === 'chat.stopStream')).toBe(true)
    expect(
      stopStreamFallbackText(calls.find(call => call.method === 'chat.stopStream')?.params)
    ).toBe('')
  })

  it('ignores duplicate terminal events after the session is already done', async () => {
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

    const first = await renderer.event(sessionId, { type: 'result', result: 'PONG' })
    const second = await renderer.event(sessionId, { type: 'turn.done', result: 'PONG' })

    const streamed = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')
    expect(streamed).toContain('PONG')
    expect(streamed.match(/PONG/g)?.length).toBe(1)
    expect(first.done).toBe(true)
    expect(second.done).toBe(true)
    expect(second.streamedAnswerChars).toBe(4)
    expect(calls.filter(call => call.method === 'chat.stopStream')).toHaveLength(1)
  })

  it('waits for turn.done when a result event has no terminal text', async () => {
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

    const emptyResult = await renderer.event(sessionId, { type: 'result' })
    const done = await renderer.event(sessionId, { type: 'turn.done', result: 'PONG' })

    const streamed = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')
    expect(emptyResult.done).toBe(false)
    expect(done.done).toBe(true)
    expect(streamed).toContain('PONG')
    expect(done.streamedAnswerChars).toBe(4)
    expect(calls.filter(call => call.method === 'chat.stopStream')).toHaveLength(1)
  })

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
    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const cmd = planTaskFromStop(calls, 'cmd-1')
    expect(cmd?.title).toBe('1. Command execution')
    expect(cmd?.status).toBe('complete')
    expect(richTextPlain(cmd?.output)).toContain('one\ntwo')
    expect(calls.filter(call => call.method === 'chat.startStream')).toHaveLength(1)
    expect(calls.some(call => call.method === 'chat.appendStream')).toBe(true)
    expect(calls.some(call => call.method === 'chat.update')).toBe(false)
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

    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const tasks = planTasksFromStop(calls)
    expect(new Set(tasks.map(task => task.task_id ?? task.id))).toEqual(new Set(['cmd-1', 'cmd-2']))
    expect(planTaskDetailsText(calls, 'cmd-1')).toContain('call demo ping')
    expect(planTaskFromStop(calls, 'cmd-2')).toMatchObject({
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
    const cmd = planTaskFromStop(calls, 'cmd-1')
    expect(cmd?.status).toBe('complete')
    expect(cmd?.title).toBe('1. Command execution')
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

    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const output = richTextPlain(planTaskFromStop(calls, 'cmd-1')?.output)
    expect(output).toContain('"tool": "grafana"')
    expect(output).not.toContain('```text')
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

    const cmd = planTaskFromStop(calls, 'cmd-1')
    const detailsText = richTextPlain(cmd?.details)
    expect(cmd?.title).toBe('1. Command execution')
    expect(detailsText).toContain('call tools')
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

    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const output = richTextPlain(planTaskFromStop(calls, 'cmd-1')?.output)
    expect(output).toContain('"demo"')
    expect(output).not.toContain('```text')
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
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

    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const toolTask = planTasksFromStop(calls).find(task => task.title === 'Use websearch')
    expect(toolTask?.title).toBe('Use websearch')
    expect(toolTask?.status).toBe('complete')
    expect(richTextPlain(toolTask?.output)).toContain('"error": "rate limited"')
    expect(toolTask?.title).not.toContain('failed')
  })

  it('streams small Thinking context before the answer after the first plan task', async () => {
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
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
    for (const char of 'abcdefghijklmnop.') {
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: char
      })
    }

    const streamedMarkdown = () =>
      calls
        .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
        .flatMap(call => call.params.chunks ?? [])
        .filter(chunk => chunk.type === 'markdown_text')
        .map(chunk => String(chunk.text))
        .join('')

    expect(streamedMarkdown()).toBe('')

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
    })

    expect(streamedMarkdown()).toBe('')

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'msg-2', type: 'agentMessage', phase: 'final_answer' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-2',
      delta: 'Done.'
    })
    await sleep(300)

    const chunks = calls.flatMap(call => call.params.chunks ?? [])
    const firstTaskIndex = chunks.findIndex(chunk => chunk.type === 'task_update')
    const thinkingIndex = chunks.findIndex(
      chunk =>
        chunk.type === 'blocks' &&
        chunk.blocks?.some((block: any) =>
          String(block.elements?.[0]?.text ?? '').includes('*Thinking*')
        )
    )
    const firstTextIndex = chunks.findIndex(
      chunk => chunk.type === 'markdown_text' && String(chunk.text).includes('Done.')
    )
    expect(firstTaskIndex).toBeGreaterThanOrEqual(0)
    expect(thinkingIndex).toBe(-1)
    expect(firstTextIndex).toBeGreaterThan(firstTaskIndex)
    expect(thinkingBlockText(calls)).toBe('')

    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    expect(streamedMarkdown()).toContain('Done.')
    expect(calls.some(call => call.method === 'chat.update')).toBe(false)
    const stop = calls.find(call => call.method === 'chat.stopStream')
    expect(stopStreamFallbackText(stop?.params).trim()).toBe('')
  })

  it('hides no-plan Thinking after the grace window when no task appears', async () => {
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
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
      delta: 'Thinking before any task.'
    })
    await sleep(550)
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: ' More thinking.'
    })

    const chunks = calls.flatMap(call => call.params.chunks ?? [])
    expect(chunks.some(chunk => chunk.type === 'plan_update')).toBe(false)
    expect(chunks.some(chunk => chunk.type === 'task_update')).toBe(false)
    expect(thinkingBlockText(calls)).toBe('')
  })

  it('does not stream hidden Thinking for hyphenated commentary', async () => {
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
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
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
    })
    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'msg-1', type: 'agentMessage', phase: 'commentary' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: 'I’m calling tools; a few may fail for auth or required'
    })
    expect(thinkingBlockText(calls)).toBe('')

    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: '-parameter reasons.'
    })

    expect(thinkingBlockText(calls)).toBe('')
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
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
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
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
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')

    expect(streamed).toContain('Done.')
    expect(thinkingBlockText(calls)).toBe('')
  })

  it('streams fenced task details in live task updates', async () => {
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
        update: async () => ({ ok: true })
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur · codex (2/2)'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping', exitCode: 0 }
    })
    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const details = planTaskDetailsText(calls, 'cmd-1')
    expect(details).toContain('```sh\ncall demo ping\n```')
  })

  it('does not resend already streamed task details on terminal updates', async () => {
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
        stopStream: async (params: any) => {
          calls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
        update: async () => ({ ok: true })
      }
    }

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur · codex'
    })
    const renderer = new CodexSessionRenderer(client as any)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping', exitCode: 0 }
    })
    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done.' })

    const updates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter((chunk: any) => chunk.type === 'task_update' && chunk.id === 'cmd-1')
    const detailUpdates = updates.filter((chunk: any) => chunk.details)
    expect(detailUpdates).toHaveLength(1)
    expect(String(detailUpdates[0]?.details ?? '')).toContain('```sh\ncall demo ping\n```')
  })

  it('preserves leading spaces between live streamed answer deltas', async () => {
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
      item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: 'hello'
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: ' world\n```ts\nconst value = 1'
    })
    const answerText = 'hello world\n```ts\nconst value = 1'
    const result = await renderer.event(sessionId, { type: 'turn.done', result: answerText })

    const visibleText = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter((chunk: any) => chunk.type === 'markdown_text')
      .map((chunk: any) => String(chunk.text ?? ''))
      .join('')
    expect(visibleText).toBe('hello world\n```ts\nconst value = 1\n```')
    expect(visibleText).not.toBe('helloworld')
    expect(result.streamedAnswerChars).toBe(answerText.length)
  })

  it('logs suspicious large answer snapshots without logging answer text', async () => {
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => {
      logCalls.push(args)
    }) as typeof console.log
    const client = {
      assistant: {
        threads: {
          setStatus: async () => ({ ok: true })
        }
      },
      chat: {
        startStream: async () => ({ ok: true, ts: '1778866940.295499' }),
        appendStream: async () => ({ ok: true }),
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
    const answer = 'first generated answer '.repeat(13)

    try {
      await renderer.event(sessionId, {
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: answer
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        centaur_thread_key: 'slack:C123:1778866921.505479',
        centaur_execution_id: 'exe-test',
        centaur_assignment_generation: 3,
        session_id: 'codex-session-test',
        item: {
          id: 'msg-1',
          type: 'agentMessage',
          phase: 'final_answer',
          text: `${answer}\n${answer}`
        }
      })
    } finally {
      console.log = originalLog
    }

    const suspiciousLogs = logCalls.filter(
      call => call[0] === 'slack_codex_suspicious_answer_delta'
    )
    expect(suspiciousLogs).toHaveLength(1)
    expect(suspiciousLogs[0]?.[1]).toMatchObject({
      agent_session_id: sessionId,
      centaur_thread_key: 'slack:C123:1778866921.505479',
      execution_id: 'exe-test',
      assignment_generation: 3,
      event_type: 'item.completed',
      codex_id: 'msg-1',
      codex_item_id: 'msg-1',
      codex_item_type: 'agentMessage',
      codex_item_phase: 'final_answer',
      codex_session_id: 'codex-session-test',
      current_answer_chars: answer.length,
      incoming_chars: answer.length * 2 + 1,
      delta_chars: answer.length + 1,
      answer_chars_after: answer.length * 2 + 1,
      current_contains_incoming_head: true,
      incoming_contains_current_tail: true,
      large_incoming_relative_to_current: true,
      large_delta_relative_to_current: true
    })
    expect(suspiciousLogs[0]?.[1]).toEqual(
      expect.objectContaining({
        current_hash: expect.any(String),
        incoming_hash: expect.any(String),
        delta_hash: expect.any(String)
      })
    )
    expect(JSON.stringify(suspiciousLogs)).not.toContain(answer.slice(0, 30))
  })

  it('does not log suspicious answer deltas for small incremental answer updates', async () => {
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => {
      logCalls.push(args)
    }) as typeof console.log
    const client = {
      assistant: {
        threads: {
          setStatus: async () => ({ ok: true })
        }
      },
      chat: {
        startStream: async () => ({ ok: true, ts: '1778866940.295499' }),
        appendStream: async () => ({ ok: true }),
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

    try {
      await renderer.event(sessionId, {
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: 'ordinary answer text '.repeat(13)
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: 'done'
      })
    } finally {
      console.log = originalLog
    }

    expect(
      logCalls.filter(call => call[0] === 'slack_codex_suspicious_answer_delta')
    ).toHaveLength(0)
  })

  it('reports only Slack-visible streamed answer chars after live text is capped', async () => {
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
    const longAnswer = 'x'.repeat(30_010)

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: longAnswer
    })
    const result = await renderer.event(sessionId, { type: 'turn.done', result: longAnswer })

    expect(result.streamedAnswerChars).toBe(30_000)
    const visibleText = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter((chunk: any) => chunk.type === 'markdown_text')
      .map((chunk: any) => String(chunk.text ?? ''))
      .join('')
    expect(visibleText.length).toBe(30_000)
  })

  it('streams commentary and answer markdown live without duplicating them on stopStream', async () => {
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
      item: { id: 'msg-1', type: 'agentMessage', phase: 'commentary' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-1',
      delta: 'Planning the tool calls.'
    })
    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'call demo ping' }
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
    await sleep(300)
    await renderer.event(sessionId, { type: 'turn.completed', result: 'Done: five tools called.' })

    const streamed = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')
    expect(streamed).toContain('Done: five tools called.')
    expect(thinkingBlockText(calls)).toBe('')

    const stop = calls.find(call => call.method === 'chat.stopStream')
    const blocks = stop?.params.blocks ?? []
    expect(stop?.params.chunks).toBeUndefined()
    expect(blocks.some((block: any) => block.type === 'context')).toBe(false)
    expect(blocks.some((block: any) => block.type === 'markdown')).toBe(false)
  })

  it('treats an unphased terminal agent message after tool use as the final answer', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => {
      logCalls.push(args)
    }) as typeof console.log
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

    try {
      await renderer.event(sessionId, {
        type: 'item.started',
        item: { id: 'msg-thinking', type: 'agentMessage', phase: 'commentary' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-thinking',
        delta: 'I’ll inspect the runtime metadata.'
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        item: {
          id: 'cmd-1',
          type: 'commandExecution',
          command: 'kubectl get pod',
          aggregated_output: 'main-sha-df02d81',
          exitCode: 0
        }
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        centaur_thread_key: 'slack:C123:1778866921.505479',
        centaur_execution_id: 'exe-test',
        centaur_assignment_generation: 3,
        session_id: 'codex-session-test',
        item: {
          id: 'msg-final',
          type: 'agentMessage',
          text: 'Staging is running `main-sha-df02d81`.'
        }
      })
      await renderer.event(sessionId, { type: 'turn.completed' })
    } finally {
      console.log = originalLog
    }

    const streamed = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter(chunk => chunk.type === 'markdown_text')
      .map(chunk => String(chunk.text))
      .join('')
    expect(streamed).toContain('Staging is running `main-sha-df02d81`.')
    expect(thinkingBlockText(calls)).not.toContain('Staging is running')
    const fallbackLogs = logCalls.filter(
      call => call[0] === 'slack_codex_unphased_final_agent_message_classified'
    )
    expect(fallbackLogs).toHaveLength(1)
    expect(fallbackLogs[0]?.[1]).toMatchObject({
      agent_session_id: sessionId,
      centaur_thread_key: 'slack:C123:1778866921.505479',
      execution_id: 'exe-test',
      assignment_generation: 3,
      codex_id: 'msg-final',
      codex_item_id: 'msg-final',
      codex_item_type: 'agentMessage',
      codex_session_id: 'codex-session-test',
      task_count: 1
    })
  })
})

function planTasksFromCalls(calls: Array<{ method: string; params: any }>): any[] {
  const stop = calls.find(call => call.method === 'chat.stopStream')
  const plan = stop?.params.blocks?.find((block: any) => block.type === 'plan')
  if (plan?.tasks?.length) return plan.tasks

  const byId = new Map<string, any>()
  for (const call of calls) {
    if (call.method !== 'chat.appendStream' && call.method !== 'chat.startStream') continue
    for (const chunk of call.params.chunks ?? []) {
      if (chunk.type !== 'task_update') continue
      const taskId = String(chunk.task_id ?? chunk.id ?? '')
      if (!taskId) continue
      byId.set(taskId, { ...byId.get(taskId), ...chunk })
    }
  }
  return Array.from(byId.values())
}

function planTasksFromStop(calls: Array<{ method: string; params: any }>): any[] {
  return planTasksFromCalls(calls)
}

function planTaskFromStop(calls: Array<{ method: string; params: any }>, id: string): any {
  return planTasksFromCalls(calls).find(task => task.task_id === id || task.id === id)
}

function planTaskDetailsText(calls: Array<{ method: string; params: any }>, id: string): string {
  for (const call of calls) {
    if (call.method !== 'chat.appendStream' && call.method !== 'chat.startStream') continue
    for (const chunk of call.params.chunks ?? []) {
      if (chunk.type !== 'task_update') continue
      if ((chunk.task_id ?? chunk.id) !== id) continue
      const text = richTextPlain(chunk.details)
      if (text) return text
    }
  }
  return richTextPlain(planTaskFromStop(calls, id)?.details)
}

function stopStreamFallbackText(params: any): string {
  return (params?.chunks ?? [])
    .filter((chunk: any) => chunk?.type === 'markdown_text')
    .map((chunk: any) => String(chunk.text ?? ''))
    .join('')
}

function thinkingBlockText(calls: Array<{ method: string; params: any }>): string {
  return calls
    .flatMap(call => call.params.chunks ?? [])
    .filter((chunk: any) => chunk.type === 'blocks')
    .flatMap((chunk: any) => chunk.blocks ?? [])
    .filter((block: any) => block.type === 'context')
    .map((block: any) => String(block.elements?.[0]?.text ?? ''))
    .join('\n')
}

async function sleep(ms: number): Promise<void> {
  await new Promise(resolve => setTimeout(resolve, ms))
}

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
