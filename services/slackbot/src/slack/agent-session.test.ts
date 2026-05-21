import { describe, expect, it } from 'bun:test'
import { AgentSessionRenderer, withAgentSessionLock } from './agent-session'

describe('AgentSessionRenderer', () => {
  it('stops calling assistant.threads.setStatus after the channel returns user_not_found', async () => {
    const setStatusCalls: any[] = []
    const client = {
      assistant: {
        threads: {
          setStatus: async (params: any) => {
            setStatusCalls.push(params)
            return { ok: false, error: 'user_not_found' }
          }
        }
      },
      chat: {
        startStream: async () => ({ ok: true, ts: '1778866940.295499' }),
        appendStream: async () => ({ ok: true }),
        stopStream: async () => ({ ok: true }),
        update: async () => ({ ok: true })
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

    expect(setStatusCalls.length).toBe(1)

    await renderer.text(sessionId, 'Hi there')
    await renderer.step(sessionId, { id: 's1', title: 'Run command', status: 'in_progress' })
    await renderer.done(sessionId)

    expect(setStatusCalls.length).toBe(1)
  })

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
    const stop = calls.find(call => call.method === 'chat.stopStream')
    expect(stopStreamFallbackText(stop?.params).trim()).toBe('')
    const taskUpdates = calls
      .flatMap(call => call.params.chunks ?? [])
      .filter((chunk: any) => chunk.type === 'task_update')
    expect(taskUpdates.at(-1)).toMatchObject({ id: 'sleep-1', status: 'complete' })
  })

  it('continues streaming when assistant status is rejected', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: {
        threads: {
          setStatus: async (params: any) => {
            calls.push({ method: 'assistant.threads.setStatus', params })
            return { ok: false, error: 'invalid_thread_ts' }
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
        update: async () => ({ ok: true })
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

    await renderer.text(sessionId, 'Reply still streams.')
    await renderer.done(sessionId)

    expect(calls.map(call => call.method)).toContain('chat.startStream')
    expect(calls.map(call => call.method)).toContain('chat.stopStream')
    expect(calls.filter(call => call.method === 'assistant.threads.setStatus')).toHaveLength(3)
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

  it('single-flights concurrent first stream updates', async () => {
    const calls: Array<{ method: string; params: any }> = []
    let resolveStart: ((value: { ok: true; ts: string }) => void) | undefined
    const startPromise = new Promise<{ ok: true; ts: string }>(resolve => {
      resolveStart = resolve
    })
    const client = {
      assistant: {
        threads: {
          setStatus: async () => ({ ok: true })
        }
      },
      chat: {
        startStream: async (params: any) => {
          calls.push({ method: 'chat.startStream', params })
          return startPromise
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

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    const first = renderer.step(sessionId, {
      id: 'cmd-1',
      title: '1. Command execution',
      status: 'in_progress'
    })
    const second = renderer.step(sessionId, {
      id: 'cmd-2',
      title: '2. Command execution',
      status: 'in_progress'
    })

    expect(calls.filter(call => call.method === 'chat.startStream')).toHaveLength(1)
    resolveStart?.({ ok: true, ts: '1778866940.295499' })
    await Promise.all([first, second])

    expect(calls.filter(call => call.method === 'chat.startStream')).toHaveLength(1)
    expect(calls.some(call => call.method === 'chat.appendStream')).toBe(true)
  })

  it('serializes work for the same agent session', async () => {
    const events: string[] = []
    let releaseFirst: (() => void) | undefined

    const first = withAgentSessionLock('session-1', async () => {
      events.push('first:start')
      await new Promise<void>(resolve => {
        releaseFirst = resolve
      })
      events.push('first:end')
    })
    const second = withAgentSessionLock('session-1', async () => {
      events.push('second:start')
    })

    await waitUntil(() => events.includes('first:start'))
    expect(events).toEqual(['first:start'])
    releaseFirst?.()
    await Promise.all([first, second])

    expect(events).toEqual(['first:start', 'first:end', 'second:start'])
  })

  it('keeps final task code blocks to four lines and preserves visible body text', async () => {
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

    await renderer.step(
      sessionId,
      {
        id: 'cmd-1',
        title: 'Run command: call workflow list',
        status: 'complete',
        details: {
          type: 'rich_text',
          elements: [
            {
              type: 'rich_text_preformatted',
              language: 'sh',
              elements: [{ type: 'text', text: 'call workflow list' }]
            }
          ]
        } as any,
        output: {
          type: 'rich_text',
          elements: [
            {
              type: 'rich_text_preformatted',
              language: 'json',
              elements: [{ type: 'text', text: '{\n  "items": [\n    1,\n    2,\n    3\n  ]\n}' }]
            }
          ]
        } as any
      },
      { flush: false }
    )
    await renderer.done(sessionId, {
      answerMarkdown: 'Final answer stays visible.'
    })

    const stop = calls.find(call => call.method === 'chat.stopStream')
    const plan = stop?.params.blocks?.find((block: any) => block.type === 'plan')
    const body = stop?.params.blocks?.find((block: any) => block.type === 'markdown')
    const outputText = plan?.tasks?.[0]?.output?.elements?.[0]?.elements?.[0]?.text ?? ''

    expect(outputText.split('\n')).toHaveLength(4)
    expect(outputText.endsWith('// truncated')).toBe(true)
    expect(body).toBeTruthy()
    expect(String(body?.text ?? '')).toContain('Final answer stays visible.')
    expect(stopStreamFallbackText(stop?.params).trim()).toBe('')
    expect(stop?.params.blocks?.length ?? 0).toBeLessThanOrEqual(50)
  })

  it('keeps a durable final plan and answer after live streaming', async () => {
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
      title: '1. Command execution',
      status: 'in_progress'
    })
    await renderer.text(sessionId, 'Live answer body.')
    await renderer.done(sessionId, {
      commentaryMarkdown: 'Planning the tool calls.',
      answerMarkdown: 'Live answer body.'
    })

    const stop = calls.find(call => call.method === 'chat.stopStream')
    const blocks = stop?.params.blocks ?? []
    expect(blocks.some((block: any) => block.type === 'plan')).toBe(true)
    expect(
      blocks.some(
        (block: any) => block.type === 'markdown' && block.text.includes('Live answer body.')
      )
    ).toBe(true)
    expect(blocks.some((block: any) => block.type === 'context')).toBe(false)
    expect(stopStreamFallbackText(stop?.params).trim()).toBe('')
    expect(calls.some(call => call.method === 'chat.appendStream')).toBe(true)
  })

  it('shows thinking text by default and renders the answer in markdown on finalize', async () => {
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

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    await renderer.done(sessionId, {
      commentaryMarkdown: 'Planning the tool calls.',
      answerMarkdown: 'Done: five tools called.'
    })

    const stop = calls.find(call => call.method === 'chat.stopStream')
    const blocks = stop?.params.blocks ?? []
    expect(
      blocks.some(
        (block: any) =>
          block.type === 'context' &&
          String(block.elements?.[0]?.text ?? '').includes('Planning the tool calls.')
      )
    ).toBe(true)
    expect(
      blocks.some(
        (block: any) =>
          block.type === 'markdown' && String(block.text).includes('Done: five tools called.')
      )
    ).toBe(true)
    expect(
      blocks.some(
        (block: any) =>
          block.type === 'markdown' && String(block.text).includes('> streamed thinking')
      )
    ).toBe(false)
  })

  it('uses clipped final answer content for fallback text on long replies', async () => {
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

    const longAnswer = 'A'.repeat(8_000)
    await renderer.text(sessionId, longAnswer)
    await renderer.done(sessionId)

    const stop = calls.find(call => call.method === 'chat.stopStream')
    const markdownBlocks = (stop?.params.blocks ?? []).filter(
      (block: any) => block.type === 'markdown'
    )
    const displayedAnswer = markdownBlocks.map((block: any) => block.text).join('\n')

    expect(stopStreamFallbackText(stop?.params).trim()).toBe('')
    if (displayedAnswer) {
      expect(displayedAnswer.length).toBeLessThan(longAnswer.length)
    }
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
    expect(renderer.done(sessionId)).rejects.toThrow('stream_already_closed')

    expect(calls.filter(call => call.method === 'assistant.threads.setStatus').at(-1)).toEqual({
      method: 'assistant.threads.setStatus',
      params: {
        channel_id: 'C123',
        thread_ts: '1778866921.505479',
        status: ''
      }
    })

    await expect(renderer.done(sessionId)).resolves.toEqual({
      streamedTextChars: expect.any(Number)
    })
    expect(stopAttempts).toBe(2)
  })

  it('prepends the persona/engine header as the first streamed chunk only', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: { threads: { setStatus: async () => ({ ok: true }) } },
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

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution',
      header: 'legal · codex-gpt-5'
    })

    await renderer.text(sessionId, 'Hello world.')
    await renderer.done(sessionId, { answerMarkdown: 'Hello world.' })

    const start = calls.find(call => call.method === 'chat.startStream')
    const firstChunk = start?.params.chunks?.[0]
    expect(firstChunk?.type).toBe('markdown_text')
    expect(String(firstChunk?.text ?? '')).toBe('_legal · codex-gpt-5_\n')

    const allStreamedChunks = calls
      .filter(call => call.method === 'chat.startStream' || call.method === 'chat.appendStream')
      .flatMap(call => call.params.chunks ?? [])
      .filter((chunk: any) => chunk?.type === 'markdown_text')
      .map((chunk: any) => String(chunk.text ?? ''))
    const headerOccurrences = allStreamedChunks.filter(text =>
      text.includes('_legal · codex-gpt-5_')
    ).length
    expect(headerOccurrences).toBe(1)

    const stop = calls.find(call => call.method === 'chat.stopStream')
    const blocks = stop?.params.blocks ?? []
    expect(
      blocks.some(
        (block: any) => block.type === 'markdown' && block.text === '_legal · codex-gpt-5_'
      )
    ).toBe(false)
  })

  it('omits the header block entirely when no header was supplied', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: { threads: { setStatus: async () => ({ ok: true }) } },
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

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    await renderer.text(sessionId, 'Hello world.')
    await renderer.done(sessionId, { answerMarkdown: 'Hello world.' })

    const start = calls.find(call => call.method === 'chat.startStream')
    expect(start?.params.chunks?.[0]?.type).not.toBe('markdown_text')
    const stop = calls.find(call => call.method === 'chat.stopStream')
    const blocks = stop?.params.blocks ?? []
    expect(
      blocks.some((block: any) => block.type === 'markdown' && /^_.*_$/.test(block.text))
    ).toBe(false)
  })

  it('renders the header above streamed tasks when both are present', async () => {
    const calls: Array<{ method: string; params: any }> = []
    const client = {
      assistant: { threads: { setStatus: async () => ({ ok: true }) } },
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

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution',
      header: 'base · claude-opus-4-7'
    })

    await renderer.step(sessionId, {
      id: 'cmd-1',
      title: 'Run command',
      status: 'in_progress',
      details: '```bash\nls\n```'
    })
    await renderer.done(sessionId)

    const start = calls.find(call => call.method === 'chat.startStream')
    const chunks = start?.params.chunks ?? []
    expect(chunks[0]).toEqual({
      type: 'markdown_text',
      text: '_base · claude-opus-4-7_\n'
    })
    const planUpdateIdx = chunks.findIndex((chunk: any) => chunk.type === 'plan_update')
    expect(planUpdateIdx).toBeGreaterThan(0)
  })
})

function stopStreamFallbackText(params: any): string {
  return (params?.chunks ?? [])
    .filter((chunk: any) => chunk?.type === 'markdown_text')
    .map((chunk: any) => String(chunk.text ?? ''))
    .join('')
}

async function waitUntil(predicate: () => boolean): Promise<void> {
  for (let index = 0; index < 10; index += 1) {
    if (predicate()) return
    await new Promise(resolve => setTimeout(resolve, 0))
  }
}
