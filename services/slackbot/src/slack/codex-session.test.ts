import { readFileSync } from 'fs'
import { join } from 'path'
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

  it('replaces the per-item buffer with item.completed canonical text and emits a correction log', async () => {
    // The May 23 2026 prod duplicate-reply bug: codex's item.completed sometimes carries
    // an item.text that differs by one interior character from the sum of streamed
    // deltas. The pre-fix delta-diff heuristic appended the entire canonical text on
    // top of the accumulated deltas, doubling the visible body.
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => logCalls.push(args)) as typeof console.log
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    const deltaSum = 'X'.repeat(500) + ' middle delta ' + 'Y'.repeat(500)
    const canonical = deltaSum.replace('middle delta', 'middle_delta')

    try {
      await renderer.event(sessionId, {
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: deltaSum
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        centaur_thread_key: 'slack:C123:1778866921.505479',
        centaur_execution_id: 'exe-test',
        centaur_assignment_generation: 3,
        session_id: 'codex-session-test',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer', text: canonical }
      })
      await renderer.event(sessionId, { type: 'turn.done', result: canonical })
    } finally {
      console.log = originalLog
    }

    const visible = visibleMarkdown(calls)
    expect(countOccurrences(visible, 'X'.repeat(500))).toBe(1)
    expect(countOccurrences(visible, 'Y'.repeat(500))).toBe(1)

    const correctionLogs = logCalls.filter(
      call => call[0] === 'slack_codex_canonical_answer_correction'
    )
    expect(correctionLogs).toHaveLength(1)
    expect(correctionLogs[0]?.[1]).toMatchObject({
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
      delta_total_chars: deltaSum.length,
      canonical_text_chars: canonical.length,
      chars_diff: 0,
      delta_hash: expect.any(String),
      canonical_hash: expect.any(String)
    })
    expect(JSON.stringify(correctionLogs)).not.toContain(deltaSum.slice(0, 20))
  })

  it('replaces a typo-corrected final snapshot instead of appending both copies', async () => {
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => logCalls.push(args)) as typeof console.log
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    const streamedDraft =
      '@alex The archive discount keeps the same 42,000 records exported, but bills eligible report jobs at a lower storage tier. For example, this looks like a single CSV export, so the metered rate should be 12 cents per thousand rows instead of 20 cents, with the difference credited after the run. This is low-risk because it changes billing, not data processing.\n\n' +
      '2) export fast path means: reduce actual work by special-casing the common “CSV export to the defaut bucket” path. Today the job does normal pre-validation, writes rows, then post-proceses checksums, touching report metadata and audit state. A fast path would skip or merge some of that when the route is trivial, for example same destination bucket, no CDN purge, pure CSV export, no mixed formats. That attacks the 17,500 extra operations directly, but it is higher-risk because it changes execution/accounting.'
    const canonical =
      '@alex The archive discount keeps the same 42,000 records exported, but bills eligible report jobs at a lower storage tier. For example, this looks like a single CSV export, so the metered rate should be 12 cents per thousand rows instead of 20 cents, with the difference credited after the run. This is low-risk because it changes billing, not data processing.\n\n' +
      '2) export fast path means: reduce actual work by special-casing the common “CSV export to the default bucket” path. Today the job does normal pre-validation, writes rows, then post-processes checksums, touching report metadata and audit state. A fast path would skip or merge some of that when the route is trivial, for example same destination bucket, no CDN purge, pure CSV export, no mixed formats. That attacks the 17,500 extra operations directly, but it is higher-risk because it changes execution/accounting.'

    try {
      await renderer.event(sessionId, {
        type: 'item.started',
        item: { id: 'msg-typo-case', type: 'agentMessage', phase: 'final_answer' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-typo-case',
        delta: streamedDraft
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        centaur_thread_key: 'slack:C123:1778866921.505479',
        centaur_execution_id: 'exe-typo-case',
        centaur_assignment_generation: 7,
        session_id: 'codex-session-typo-case',
        item: {
          id: 'msg-typo-case',
          type: 'agentMessage',
          phase: 'final_answer',
          text: canonical
        }
      })
      await renderer.event(sessionId, { type: 'turn.done', result: canonical })
    } finally {
      console.log = originalLog
    }

    const visible = visibleMarkdown(calls)
    expect(countOccurrences(visible, '@alex The archive discount')).toBe(1)
    expect(visible).toContain('default bucket')
    expect(visible).toContain('post-processes checksums')
    expect(visible).not.toContain('defaut bucket')
    expect(visible).not.toContain('post-proceses checksums')

    const correctionLogs = logCalls.filter(
      call => call[0] === 'slack_codex_canonical_answer_correction'
    )
    expect(correctionLogs).toHaveLength(1)
    expect(correctionLogs[0]?.[1]).toMatchObject({
      agent_session_id: sessionId,
      centaur_thread_key: 'slack:C123:1778866921.505479',
      execution_id: 'exe-typo-case',
      assignment_generation: 7,
      event_type: 'item.completed',
      codex_id: 'msg-typo-case',
      delta_total_chars: streamedDraft.length,
      canonical_text_chars: canonical.length,
      chars_diff: canonical.length - streamedDraft.length,
      delta_hash: expect.any(String),
      canonical_hash: expect.any(String)
    })
  })

  it('reports canonical completed snapshot chars after filling missing live text', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    const streamedDraft = 'The report starts here.'
    const canonical = `${streamedDraft} This sentence only appears in item.completed.`

    await renderer.event(sessionId, {
      type: 'item.started',
      item: { id: 'msg-missing-live-text', type: 'agentMessage', phase: 'final_answer' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-missing-live-text',
      delta: streamedDraft
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'msg-missing-live-text',
        type: 'agentMessage',
        phase: 'final_answer',
        text: canonical
      }
    })
    const result = await renderer.event(sessionId, { type: 'turn.done', result: canonical })

    const visible = visibleMarkdown(calls)
    expect(countOccurrences(visible, streamedDraft)).toBe(1)
    expect(visible).toContain('This sentence only appears in item.completed.')
    expect(result.streamedAnswerChars).toBe(canonical.length)
  })

  it('replays the exact prod event stream from exe_a89da7f248bb4724 without doubling the reply', async () => {
    // Real captured event stream from the May 23 2026 prod duplicate-reply incident.
    // Pre-fix, this exact sequence produced a Slack message that contained the entire
    // 1540-char final answer twice (visible body ~3079 chars). With the per-item
    // canonical-replace contract, the final answer appears exactly once.
    const fixturePath = join(
      __dirname,
      '..',
      '..',
      'test-fixtures',
      'codex',
      'exe_a89da7f248bb4724-min.json'
    )
    const fixture = JSON.parse(readFileSync(fixturePath, 'utf8')) as {
      events: Array<Record<string, unknown>>
    }
    const finalEvent = fixture.events.find(
      (event: any) =>
        event.type === 'item.completed' &&
        event?.item?.type === 'agentMessage' &&
        event?.item?.phase === 'final_answer'
    ) as { item: { text: string } } | undefined
    expect(finalEvent).toBeTruthy()
    const canonicalAnswer = finalEvent!.item.text
    expect(canonicalAnswer.length).toBeGreaterThan(1000)

    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)

    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    for (const event of fixture.events) {
      await renderer.event(sessionId, event)
    }

    const visible = visibleMarkdown(calls)
    const openingPhrase = canonicalAnswer.slice(0, 80)
    expect(countOccurrences(canonicalAnswer, openingPhrase)).toBe(1)
    expect(countOccurrences(visible, openingPhrase)).toBe(1)
    const closingPhrase = canonicalAnswer.slice(-80)
    expect(countOccurrences(canonicalAnswer, closingPhrase)).toBe(1)
    expect(countOccurrences(visible, closingPhrase)).toBe(1)
  })

  it('concatenates two final_answer agentMessages in the same turn exactly once each', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
    const { sessionId } = await new AgentSessionRenderer(client as any).open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })
    const renderer = new CodexSessionRenderer(client as any)

    for (const id of ['msg-a', 'msg-b']) {
      await renderer.event(sessionId, {
        type: 'item.started',
        item: { id, type: 'agentMessage', phase: 'final_answer' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: id,
        delta: `Half ${id}.`
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        item: { id, type: 'agentMessage', phase: 'final_answer', text: `Half ${id}.` }
      })
    }
    await renderer.event(sessionId, { type: 'turn.done', result: 'Half msg-a.Half msg-b.' })

    const visible = visibleMarkdown(calls)
    expect(countOccurrences(visible, 'Half msg-a.')).toBe(1)
    expect(countOccurrences(visible, 'Half msg-b.')).toBe(1)
  })

  it('ignores deltas that arrive after the same item has already been completed', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
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
      item: { id: 'msg-late', type: 'agentMessage', phase: 'final_answer' }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-late',
      delta: 'Final canonical reply.'
    })
    await renderer.event(sessionId, {
      type: 'item.completed',
      item: {
        id: 'msg-late',
        type: 'agentMessage',
        phase: 'final_answer',
        text: 'Final canonical reply.'
      }
    })
    await renderer.event(sessionId, {
      type: 'item.agentMessage.delta',
      itemId: 'msg-late',
      delta: ' INJECTED-LATE-CHUNK'
    })
    await renderer.event(sessionId, { type: 'turn.done', result: 'Final canonical reply.' })

    const visible = visibleMarkdown(calls)
    expect(visible).not.toContain('INJECTED-LATE-CHUNK')
    expect(countOccurrences(visible, 'Final canonical reply.')).toBe(1)
  })

  it('drops item.completed without an item id and emits a missing-id log', async () => {
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => logCalls.push(args)) as typeof console.log
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
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
        item: { id: 'msg-pre', type: 'agentMessage', phase: 'final_answer' }
      })
      await renderer.event(sessionId, {
        type: 'item.agentMessage.delta',
        itemId: 'msg-pre',
        delta: 'Streamed reply.'
      })
      await renderer.event(sessionId, {
        type: 'item.completed',
        centaur_thread_key: 'slack:C123:1778866921.505479',
        centaur_execution_id: 'exe-noid',
        item: { type: 'agentMessage', phase: 'final_answer', text: 'Malformed canonical body.' }
      })
      await renderer.event(sessionId, { type: 'turn.done', result: 'Streamed reply.' })
    } finally {
      console.log = originalLog
    }

    const visible = visibleMarkdown(calls)
    expect(visible).toContain('Streamed reply.')
    expect(visible).not.toContain('Malformed canonical body.')

    const missingIdLogs = logCalls.filter(
      call => call[0] === 'slack_codex_item_completed_missing_id'
    )
    expect(missingIdLogs).toHaveLength(1)
    expect(missingIdLogs[0]?.[1]).toMatchObject({
      agent_session_id: sessionId,
      centaur_thread_key: 'slack:C123:1778866921.505479',
      execution_id: 'exe-noid',
      canonical_text_chars: 'Malformed canonical body.'.length,
      canonical_hash: expect.any(String)
    })
  })

  it('streams cumulative amp/claude assistant text without duplication', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
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
      message: { content: [{ type: 'text', text: 'Hello.' }] }
    })
    await renderer.event(sessionId, {
      type: 'assistant',
      message: { content: [{ type: 'text', text: 'Hello. World.' }] }
    })
    await renderer.event(sessionId, { type: 'result', result: 'Hello. World.' })

    const visible = visibleMarkdown(calls)
    expect(countOccurrences(visible, 'Hello. World.')).toBe(1)
    expect(countOccurrences(visible, 'Hello.')).toBe(1)
  })

  it('does not log a canonical correction for plain delta streams without item.completed', async () => {
    const logCalls: unknown[][] = []
    const originalLog = console.log
    console.log = ((...args: unknown[]) => logCalls.push(args)) as typeof console.log
    const calls: Array<{ method: string; params: any }> = []
    const client = makeFakeSlackClient(calls)
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
      logCalls.filter(call => call[0] === 'slack_codex_canonical_answer_correction')
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

function countOccurrences(haystack: string, needle: string): number {
  if (!needle) return 0
  let count = 0
  let index = 0
  while (true) {
    const at = haystack.indexOf(needle, index)
    if (at === -1) return count
    count += 1
    index = at + needle.length
  }
}

function visibleMarkdown(calls: Array<{ method: string; params: any }>): string {
  const streamed = calls
    .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
    .flatMap(call => call.params.chunks ?? [])
    .filter((chunk: any) => chunk.type === 'markdown_text')
    .map((chunk: any) => String(chunk.text ?? ''))
    .join('')
  const stopBlocks = calls.find(call => call.method === 'chat.stopStream')?.params.blocks ?? []
  const stopMarkdown = stopBlocks
    .filter((block: any) => block.type === 'markdown')
    .map((block: any) => String(block.text ?? ''))
    .join('')
  return streamed + stopMarkdown
}

function makeFakeSlackClient(
  calls: Array<{ method: string; params: any }>
): Record<string, unknown> {
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
}
