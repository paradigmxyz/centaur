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

    await renderer.text(sessionId, 'Final answer stays visible.')
    await renderer.step(sessionId, {
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
    })
    await renderer.done(sessionId)

    const update = calls.find(call => call.method === 'chat.update')
    const plan = update?.params.blocks?.find((block: any) => block.type === 'plan')
    const body = update?.params.blocks?.find((block: any) => block.type === 'markdown')
    const outputText = plan?.tasks?.[0]?.output?.elements?.[0]?.elements?.[0]?.text ?? ''

    expect(outputText.split('\n')).toHaveLength(4)
    expect(outputText.endsWith('// truncated')).toBe(true)
    expect(body).toBeTruthy()
    expect(update?.params.text).toContain('Final answer stays visible.')
    expect((update?.params.text ?? '').length).toBeLessThanOrEqual(4_000)
    expect(update?.params.blocks?.length ?? 0).toBeLessThanOrEqual(50)
  })

  it('renders thinking in a context block and the answer in markdown on finalize', async () => {
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

    const renderer = new AgentSessionRenderer(client as any)
    const { sessionId } = await renderer.open({
      channel: 'C123',
      parentTs: '1778866921.505479',
      recipientTeamId: 'T123',
      recipientUserId: 'U123',
      title: 'Centaur execution'
    })

    await renderer.text(sessionId, '> streamed thinking')
    await renderer.done(sessionId, 'Codex thread `T-1`', {
      commentaryMarkdown: 'Planning the tool calls.',
      answerMarkdown: 'Done: five tools called.'
    })

    const update = calls.find(call => call.method === 'chat.update')
    const blocks = update?.params.blocks ?? []
    expect(
      blocks.some(
        (block: any) =>
          block.type === 'context' &&
          String(block.elements?.[0]?.text ?? '').includes('*Thinking*') &&
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
    await renderer.done(sessionId, 'Codex thread `T-1`')

    const update = calls.find(call => call.method === 'chat.update')
    const markdownBlocks = (update?.params.blocks ?? []).filter(
      (block: any) => block.type === 'markdown'
    )
    const displayedAnswer = markdownBlocks.map((block: any) => block.text).join('\n')

    expect((update?.params.text ?? '').length).toBeLessThanOrEqual(4_000)
    expect(update?.params.text).not.toBe(longAnswer)
    if (displayedAnswer) {
      expect(update?.params.text).toContain(displayedAnswer.slice(0, 200))
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

    expect(calls.at(-1)).toEqual({
      method: 'assistant.threads.setStatus',
      params: {
        channel_id: 'C123',
        thread_ts: '1778866921.505479',
        status: ''
      }
    })

    expect(renderer.done(sessionId)).resolves.toBeUndefined()
    expect(stopAttempts).toBe(2)
  })
})
