import { describe, expect, it } from 'bun:test'
import {
  CodexAppServerRendererEventMapper,
  codexAppServerToChatSdkStream,
  codexAppServerToRendererEvents
} from './codex-app-server'
import type { RendererTaskBlock } from './types'

describe('CodexAppServerRendererEventMapper', () => {
  it('maps final answer deltas to generic renderer message deltas after activity exists', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    const commandEvents = mapper.process({
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'pnpm test' }
    })
    expect(commandEvents).toContainEqual({
      type: 'renderer.task.update',
      task: {
        id: 'cmd-1',
        title: '1. Command execution',
        status: 'in_progress',
        details: [
          {
            type: 'code',
            language: 'sh',
            text: 'pnpm test'
          }
        ],
        output: undefined
      },
      flush: true
    })

    expect(
      mapper.process({
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      })
    ).toEqual([])

    expect(
      mapper.process({
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: 'Done.'
      })
    ).toContainEqual({
      type: 'renderer.message.delta',
      delta: 'Done.',
      force: false,
      planPrefix: true
    })
  })

  it('maps commentary to Thinking task updates instead of message deltas', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    mapper.process({
      type: 'item.started',
      item: { id: 'thinking-1', type: 'agentMessage', phase: 'commentary' }
    })
    mapper.process({
      type: 'item.agentMessage.delta',
      itemId: 'thinking-1',
      delta: 'Checking the runtime.'
    })

    const events = mapper.process({
      type: 'item.completed',
      item: {
        id: 'thinking-1',
        type: 'agentMessage',
        phase: 'commentary',
        text: 'Checking the runtime.'
      }
    })

    expect(events.some(event => event.type === 'renderer.message.delta')).toBe(false)
    const task = events.find(event => event.type === 'renderer.task.update')
    // Sealed commentary stays in_progress until the next activity starts so
    // "Thinking completed" never headlines the Slack plan card mid-turn.
    expect(task).toMatchObject({
      type: 'renderer.task.update',
      task: {
        id: 'thinking-thinking-1',
        title: 'Thinking',
        status: 'in_progress'
      }
    })
    expect(plain(task?.type === 'renderer.task.update' ? task.task.details : undefined)).toContain(
      'Checking the runtime.'
    )

    const next = mapper.process({
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'pnpm test' }
    })
    expect(next.find(event => event.type === 'renderer.task.update')).toMatchObject({
      task: { id: 'thinking-thinking-1', title: 'Thinking', status: 'complete' }
    })
  })

  it('keeps one Thinking task in_progress across reasoning deltas until the item seals', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    const first = mapper.process({
      type: 'item.reasoning.textDelta',
      itemId: 'reasoning-1',
      delta: 'Inspecting the '
    })
    expect(first).toContainEqual({
      type: 'renderer.task.update',
      task: {
        id: 'reasoning-1',
        title: 'Thinking',
        status: 'in_progress',
        details: [{ type: 'text', text: 'Inspecting the' }],
        output: undefined
      },
      flush: true
    })

    // A command starting mid-thought must not flip the Thinking task to complete.
    mapper.process({
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'pnpm test' }
    })

    const second = mapper.process({
      type: 'item.reasoning.textDelta',
      itemId: 'reasoning-1',
      delta: 'event stream'
    })
    const secondUpdate = second.find(event => event.type === 'renderer.task.update')
    expect(secondUpdate).toMatchObject({
      task: { id: 'reasoning-1', title: 'Thinking', status: 'in_progress' }
    })
    expect(
      plain(secondUpdate?.type === 'renderer.task.update' ? secondUpdate.task.details : undefined)
    ).toContain('Inspecting the event stream')

    // Sealing completes the Thinking task; the still-running command keeps
    // the plan in an in-progress state so the Slack header tracks it.
    const sealed = mapper.process({
      type: 'item.completed',
      item: { id: 'reasoning-1', type: 'reasoning', content: ['Inspecting the event stream'] }
    })
    const sealedUpdate = sealed.find(event => event.type === 'renderer.task.update')
    expect(sealedUpdate).toMatchObject({
      task: { id: 'reasoning-1', title: 'Thinking', status: 'complete' }
    })
  })

  it('holds the last finished task in_progress so the Slack header never claims completion mid-turn', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    mapper.process({
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'pnpm test' }
    })

    // The command finishes, leaving nothing else running. Slack would show
    // "Thinking completed" for an all-complete plan, so the completion is
    // held back and the task stays presented as in_progress.
    const completed = mapper.process({
      type: 'item.completed',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'pnpm test',
        aggregatedOutput: 'tests passed',
        status: 'completed'
      }
    })
    const heldUpdate = completed.find(event => event.type === 'renderer.task.update')
    expect(heldUpdate).toMatchObject({
      task: { id: 'cmd-1', status: 'in_progress' }
    })

    // The next command releases the held completion in the same batch,
    // ordered before the new task's in_progress update.
    const next = mapper.process({
      type: 'item.started',
      item: { id: 'cmd-2', type: 'commandExecution', command: 'pnpm build' }
    })
    const updates = next.filter(event => event.type === 'renderer.task.update')
    const firstIndex = updates.findIndex(
      update => update.type === 'renderer.task.update' && update.task.id === 'cmd-1'
    )
    const secondIndex = updates.findIndex(
      update => update.type === 'renderer.task.update' && update.task.id === 'cmd-2'
    )
    expect(updates[firstIndex]).toMatchObject({ task: { id: 'cmd-1', status: 'complete' } })
    expect(updates[secondIndex]).toMatchObject({ task: { id: 'cmd-2', status: 'in_progress' } })
    expect(firstIndex).toBeLessThan(secondIndex)

    // The final flush reports true statuses for everything.
    const done = mapper.flush()
    const finalStatuses = done
      .filter(event => event.type === 'renderer.task.update')
      .map(event => (event.type === 'renderer.task.update' ? event.task : null))
    expect(finalStatuses).toContainEqual(
      expect.objectContaining({ id: 'cmd-2', status: 'complete' })
    )
  })

  it('separates Codex reasoning summary sections within one Thinking task', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    mapper.process({
      type: 'item.reasoning.summaryTextDelta',
      itemId: 'reasoning-1',
      summaryIndex: 0,
      delta: 'First section.'
    })
    const events = mapper.process({
      type: 'item.reasoning.summaryTextDelta',
      itemId: 'reasoning-1',
      summaryIndex: 1,
      delta: 'Second section.'
    })
    const update = events.find(event => event.type === 'renderer.task.update')
    expect(update).toMatchObject({
      task: {
        id: 'reasoning-1',
        title: 'Thinking',
        status: 'in_progress',
        details: [{ type: 'text', text: 'First section.\n\nSecond section.' }]
      }
    })
  })

  it('parses Rust session output lines before mapping app-server notifications', () => {
    const mapper = new CodexAppServerRendererEventMapper()
    mapper.process({
      eventKind: 'session.output.line',
      data: JSON.stringify({
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      })
    })
    const events = mapper.process({
      eventKind: 'session.output.line',
      data: JSON.stringify({
        type: 'turn.done',
        result: 'PONG'
      })
    })

    expect(events).toContainEqual({
      type: 'renderer.message.delta',
      delta: 'PONG',
      force: true,
      planPrefix: false
    })
    expect(events.at(-1)).toMatchObject({
      type: 'renderer.done',
      answerMarkdown: 'PONG'
    })
  })

  it('hydrates final answer text from Rust execution completion payloads', () => {
    const mapper = new CodexAppServerRendererEventMapper()
    mapper.process({
      eventKind: 'session.output.line',
      data: JSON.stringify({
        type: 'item.started',
        item: { id: 'cmd-1', type: 'commandExecution', command: 'true' }
      })
    })

    const events = mapper.process({
      eventKind: 'session.execution_completed',
      data: {
        execution_id: 'exe-1',
        result_text: 'TERMINAL_RESULT_OK'
      }
    })

    expect(events).toContainEqual({
      type: 'renderer.message.delta',
      delta: 'TERMINAL_RESULT_OK',
      force: true,
      planPrefix: true
    })
    expect(events.at(-1)).toMatchObject({
      type: 'renderer.done',
      answerMarkdown: 'TERMINAL_RESULT_OK'
    })
  })

  it('maps app-server agent message deltas keyed by turnId', () => {
    const mapper = new CodexAppServerRendererEventMapper()
    const events = mapper.process({
      eventKind: 'session.output.line',
      data: JSON.stringify({
        type: 'item.agentMessage.delta',
        turnId: 'turn-1',
        delta: 'PONG 1'
      })
    })

    expect(mapper.flush()).toContainEqual({
      type: 'renderer.message.delta',
      delta: 'PONG 1',
      force: true,
      planPrefix: false
    })
    expect(events).toEqual([])
  })

  it('accepts already-parsed Rust session output payloads from API clients', () => {
    const mapper = new CodexAppServerRendererEventMapper()
    mapper.process({
      eventKind: 'session.output.line',
      data: {
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      }
    })

    const events = mapper.process({
      eventKind: 'session.output.line',
      data: {
        type: 'turn.done',
        result: 'PONG'
      }
    })

    expect(events).toContainEqual({
      type: 'renderer.message.delta',
      delta: 'PONG',
      force: true,
      planPrefix: false
    })
  })

  it('maps thread name updates without making them Slack-specific', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    expect(
      mapper.process({
        type: 'thread/name/updated',
        name: 'Investigate staging deploy'
      })
    ).toEqual([{ type: 'renderer.title.update', title: 'Investigate staging deploy' }])
  })

  it('accepts App Server slash-method notifications from Slackbotv2 streams', async () => {
    const titles: string[] = []
    const chunks = await collect(
      codexAppServerToChatSdkStream(
        toAsyncIterable([
          {
            method: 'thread/name/updated',
            params: { threadId: 'thread-1', threadName: 'Investigate staging deploy' }
          },
          {
            method: 'turn/plan/updated',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              explanation: 'Implementation plan',
              plan: [{ step: 'Inspect App Server events', status: 'completed' }]
            }
          },
          {
            method: 'item/reasoning/summaryTextDelta',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              itemId: 'reasoning-1',
              summaryIndex: 0,
              delta: 'Inspecting the event stream'
            }
          },
          {
            method: 'item/agentMessage/delta',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              itemId: 'answer-1',
              delta: 'Done.'
            }
          },
          {
            method: 'turn/completed',
            params: {
              threadId: 'thread-1',
              turn: { id: 'turn-1', items: [], status: 'completed' }
            }
          }
        ]),
        {
          onRendererEvent(event) {
            if (event.type === 'renderer.title.update') titles.push(event.title)
          }
        }
      )
    )

    expect(titles).toEqual(['Investigate staging deploy'])
    expect(chunks).toContainEqual({ type: 'plan_update', title: 'Implementation plan' })
    expect(chunks).toContainEqual({
      type: 'task_update',
      id: 'plan-1',
      title: 'Inspect App Server events',
      status: 'complete'
    })
    expect(chunks).toContainEqual({
      type: 'task_update',
      id: 'reasoning-1',
      title: 'Thinking',
      status: 'in_progress',
      details: 'Inspecting the event stream'
    })
    expect(chunks).toContainEqual({
      type: 'task_update',
      id: 'reasoning-1',
      title: 'Thinking',
      status: 'complete'
    })
    expect(chunks).toContainEqual({ type: 'markdown_text', text: 'Done.' })
  })

  it('coalesces repeated reasoning deltas into one Thinking task', async () => {
    const chunks = await collect(
      codexAppServerToChatSdkStream(
        toAsyncIterable([
          {
            method: 'item/reasoning/summaryTextDelta',
            params: {
              itemId: 'reasoning-1',
              delta: 'Inspecting '
            }
          },
          {
            method: 'item/reasoning/summaryTextDelta',
            params: {
              itemId: 'reasoning-1',
              delta: 'the event stream'
            }
          },
          {
            method: 'turn/completed',
            params: {
              turn: { id: 'turn-1', items: [], status: 'completed' }
            }
          }
        ])
      )
    )

    const thinkingChunks = chunks.filter(
      (chunk): chunk is Extract<(typeof chunks)[number], { type: 'task_update' }> =>
        chunk.type === 'task_update' && chunk.title === 'Thinking'
    )
    expect(new Set(thinkingChunks.map(chunk => chunk.id))).toEqual(new Set(['reasoning-1']))
    expect(thinkingChunks).toContainEqual({
      type: 'task_update',
      id: 'reasoning-1',
      title: 'Thinking',
      status: 'in_progress',
      details: 'Inspecting the event stream'
    })
    expect(thinkingChunks).toContainEqual({
      type: 'task_update',
      id: 'reasoning-1',
      title: 'Thinking',
      status: 'complete'
    })
  })

  it('streams command details once and command output incrementally', async () => {
    const chunks = await collect(
      codexAppServerToChatSdkStream(
        toAsyncIterable([
          {
            method: 'item/started',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              item: {
                id: 'cmd-1',
                type: 'commandExecution',
                command: 'echo one && echo two',
                status: 'inProgress'
              }
            }
          },
          {
            method: 'item/commandExecution/outputDelta',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              itemId: 'cmd-1',
              delta: 'one\n'
            }
          },
          {
            method: 'item/commandExecution/outputDelta',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              itemId: 'cmd-1',
              delta: 'two\n'
            }
          },
          {
            method: 'item/completed',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              item: {
                id: 'cmd-1',
                type: 'commandExecution',
                command: 'echo one && echo two',
                status: 'completed',
                aggregatedOutput: 'one\ntwo\n',
                exitCode: 0
              }
            }
          }
        ])
      )
    )

    const taskChunks = chunks.filter(
      (chunk): chunk is Extract<(typeof chunks)[number], { type: 'task_update' }> =>
        chunk.type === 'task_update' && chunk.id === 'cmd-1'
    )
    expect(taskChunks.filter(chunk => chunk.details).map(chunk => chunk.details)).toEqual([
      '```sh\necho one && echo two\n```'
    ])
    expect(taskChunks.filter(chunk => chunk.output).map(chunk => chunk.output)).toEqual([
      'one\n',
      'two\n'
    ])
    expect(taskChunks.at(-1)).toMatchObject({
      id: 'cmd-1',
      status: 'complete'
    })
  })

  it('preserves full command output in task updates', async () => {
    const longSuffix = 'x'.repeat(13_000)
    const aggregatedOutput = `one\ntwo\nthree\nfour\nfive\n${longSuffix}`
    const chunks = await collect(
      codexAppServerToChatSdkStream(
        toAsyncIterable([
          {
            method: 'item/completed',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              item: {
                id: 'cmd-1',
                type: 'commandExecution',
                command: 'cat long-output.log',
                status: 'completed',
                aggregatedOutput,
                exitCode: 0
              }
            }
          }
        ])
      )
    )

    const taskChunk = chunks.find(
      (chunk): chunk is Extract<(typeof chunks)[number], { type: 'task_update' }> =>
        chunk.type === 'task_update' && chunk.id === 'cmd-1'
    )
    expect(taskChunk?.output).toContain(aggregatedOutput)
    expect(taskChunk?.output).not.toContain('// truncated')
    expect(taskChunk?.output).not.toContain('/* truncated */')
    expect(taskChunk?.output).not.toContain('[output truncated]')
    expect(taskChunk?.output).not.toContain('[truncated')
  })

  it('preserves full file change diffs in task updates', async () => {
    const longLine = `+${'diff-line'.repeat(400)}`
    const diff = ['diff --git a/file.txt b/file.txt', '@@ -1 +1 @@', '-old', longLine].join('\n')
    const chunks = await collect(
      codexAppServerToChatSdkStream(
        toAsyncIterable([
          {
            method: 'item/completed',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              item: {
                id: 'file-1',
                type: 'fileChange',
                status: 'completed',
                changes: [{ path: 'file.txt', diff }]
              }
            }
          }
        ])
      )
    )

    const taskChunk = chunks.find(
      (chunk): chunk is Extract<(typeof chunks)[number], { type: 'task_update' }> =>
        chunk.type === 'task_update' && chunk.id === 'file-1'
    )
    expect(taskChunk?.output).toContain(diff)
    expect(taskChunk?.output).not.toContain('// truncated')
    expect(taskChunk?.output).not.toContain('/* truncated */')
    expect(taskChunk?.output).not.toContain('[output truncated]')
    expect(taskChunk?.output).not.toContain('[truncated')
  })

  it('omits binary command output from task updates', async () => {
    const chunks = await collect(
      codexAppServerToChatSdkStream(
        toAsyncIterable([
          {
            method: 'item/completed',
            params: {
              threadId: 'thread-1',
              turnId: 'turn-1',
              item: {
                id: 'cmd-1',
                type: 'commandExecution',
                command: 'head -40 $(which centaur-tools)',
                status: 'completed',
                aggregatedOutput: `ELF\u0000\u0001\u0002\u0003${'\u0004'.repeat(16)}`,
                exitCode: 0
              }
            }
          }
        ])
      )
    )

    const taskChunk = chunks.find(
      (chunk): chunk is Extract<(typeof chunks)[number], { type: 'task_update' }> =>
        chunk.type === 'task_update' && chunk.id === 'cmd-1'
    )
    expect(taskChunk?.output).toContain('[binary output omitted;')
    expect(taskChunk?.output).not.toContain('\u0000')
  })

  it('suppresses the live delta instead of interleaving when the recomposed answer diverges from streamed text', () => {
    // Two concurrent final-answer items compose as A + B. Growing A (a
    // non-trailing component) after B was already streamed shifts B's bytes, so
    // a byte-offset slice would re-read and re-emit B, duplicating it
    // ("Hello world." -> "Hello world.world."). The guard refuses the
    // non-continuation and freezes at the clean prefix instead, and records the
    // divergence once for the metric.
    const logs: string[] = []
    const mapper = new CodexAppServerRendererEventMapper({
      logInfo: event => logs.push(event)
    })

    const deltas: string[] = []
    const run = (event: unknown) => {
      for (const out of mapper.process(event)) {
        if (out.type === 'renderer.message.delta') deltas.push(out.delta)
      }
    }

    // A task makes the plan visible so answer deltas stream immediately.
    run({ type: 'item.started', item: { id: 'cmd-1', type: 'commandExecution', command: 'true' } })
    run({ type: 'item.started', item: { id: 'A', type: 'agentMessage', phase: 'final_answer' } })
    run({ type: 'item.started', item: { id: 'B', type: 'agentMessage', phase: 'final_answer' } })
    run({ type: 'item.agentMessage.delta', itemId: 'A', delta: 'Hello ' })
    run({ type: 'item.agentMessage.delta', itemId: 'B', delta: 'world.' })
    // A grows after B was already streamed: a byte-offset slice would duplicate
    // "world." here. The guard suppresses the non-continuation instead.
    run({ type: 'item.agentMessage.delta', itemId: 'A', delta: 'there ' })

    const streamed = deltas.join('')
    expect(streamed).toBe('Hello world.')
    expect(streamed).not.toContain('world.world.')
    expect(logs).toContain('codex_renderer_stream_divergence_suppressed')
    // The signal is recorded once per render, not on every subsequent event.
    expect(logs.filter(event => event === 'codex_renderer_stream_divergence_suppressed')).toHaveLength(
      1
    )
  })

  it('marks open tasks as errors on Rust session failures and emits done', () => {
    const mapper = new CodexAppServerRendererEventMapper()
    mapper.process({
      type: 'item.started',
      item: { id: 'cmd-1', type: 'commandExecution', command: 'kubectl get pods' }
    })

    const events = mapper.process({
      eventKind: 'session.execution_failed',
      data: { error: 'sandbox exited' }
    })

    expect(events).toContainEqual({
      type: 'renderer.task.update',
      task: {
        id: 'cmd-1',
        title: '1. Command execution',
        status: 'error',
        details: undefined,
        output: undefined
      },
      flush: true
    })
    expect(events.at(-1)).toMatchObject({
      type: 'renderer.done',
      error: 'sandbox exited'
    })
  })
})

function plain(elements: RendererTaskBlock[] | undefined): string {
  return (elements ?? [])
    .map(element => element.text)
    .join('')
}

async function collect<T>(source: AsyncIterable<T>): Promise<T[]> {
  const out: T[] = []
  for await (const item of source) out.push(item)
  return out
}

async function* toAsyncIterable<T>(source: Iterable<T>): AsyncIterable<T> {
  for (const item of source) yield item
}

describe('codexAppServerToRendererEvents', () => {
  it('flushes buffered answer text and emits renderer.done when the source ends without a terminal event', () => {
    const events = codexAppServerToRendererEvents([
      {
        type: 'item.started',
        item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
      },
      {
        type: 'item.agentMessage.delta',
        itemId: 'msg-1',
        delta: 'Final answer text.'
      }
    ])

    const done = events.find(event => event.type === 'renderer.done')
    expect(done).toMatchObject({
      type: 'renderer.done',
      answerMarkdown: 'Final answer text.'
    })
  })
})
