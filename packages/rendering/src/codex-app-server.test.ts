import { describe, expect, it } from 'bun:test'
import { CodexAppServerRendererEventMapper, codexAppServerToChatSdkStream } from './codex-app-server'
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
    expect(task).toMatchObject({
      type: 'renderer.task.update',
      task: {
        id: 'thinking-thinking-1',
        title: 'Thinking',
        status: 'complete'
      }
    })
    expect(plain(task?.type === 'renderer.task.update' ? task.task.details : undefined)).toContain(
      'Checking the runtime.'
    )
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

  it('emits a readable fallback when a completed turn has no final answer text', () => {
    const mapper = new CodexAppServerRendererEventMapper()

    mapper.process({
      type: 'item.started',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'printf done',
        status: 'inProgress'
      }
    })
    mapper.process({
      type: 'item.completed',
      item: {
        id: 'cmd-1',
        type: 'commandExecution',
        command: 'printf done',
        status: 'completed',
        aggregatedOutput: 'done\n'
      }
    })
    mapper.process({
      type: 'item.started',
      item: { id: 'msg-1', type: 'agentMessage', phase: 'final_answer' }
    })

    const events = mapper.process({
      type: 'turn.completed',
      turn: { id: 'turn-1', items: [], status: 'completed' }
    })

    expect(events).toContainEqual({
      type: 'renderer.message.delta',
      delta: 'Execution completed, but no final text was captured.',
      force: true,
      planPrefix: true
    })
    expect(events.at(-1)).toMatchObject({
      type: 'renderer.done',
      answerMarkdown: 'Execution completed, but no final text was captured.'
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
    expect(taskChunks.filter(chunk => chunk.output).map(chunk => chunk.output)).toEqual(['one\n', 'two\n'])
    expect(taskChunks.at(-1)).toMatchObject({
      id: 'cmd-1',
      status: 'complete'
    })
  })

  it('marks nonzero commands as errors without prefixing exit code into output', async () => {
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
                command: "bash -lc 'echo before; false'",
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
              delta: 'before\n'
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
                command: "bash -lc 'echo before; false'",
                status: 'completed',
                aggregatedOutput: 'before\n',
                exitCode: 1
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
    expect(taskChunks.filter(chunk => chunk.output).map(chunk => chunk.output)).toEqual(['before\n'])
    expect(taskChunks.map(chunk => chunk.output).join('\n')).not.toContain('exit code')
    expect(taskChunks.at(-1)).toMatchObject({
      id: 'cmd-1',
      status: 'error'
    })
  })

  it('redacts credential-shaped command details and output', async () => {
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
                command:
                  'curl -H "Authorization: Bearer sbx1.threadpayload.signature" https://example.test',
                status: 'inProgress'
              }
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
                command:
                  'curl -H "Authorization: Bearer sbx1.threadpayload.signature" https://example.test',
                status: 'completed',
                aggregatedOutput:
                  'agent curl -H Authorization: Bearer sbx1.threadpayload.signature\nCENTAUR_API_KEY=sbx1.otherpayload.othersig\nSLACK_BOT_TOKEN=xoxb-1234567890-abcdef\n',
                exitCode: 0
              }
            }
          }
        ])
      )
    )

    const rendered = chunks
      .filter((chunk): chunk is Extract<(typeof chunks)[number], { type: 'task_update' }> =>
        chunk.type === 'task_update' && chunk.id === 'cmd-1'
      )
      .map(chunk => `${chunk.details ?? ''}\n${chunk.output ?? ''}`)
      .join('\n')

    expect(rendered).not.toContain('sbx1.')
    expect(rendered).not.toContain('xoxb-')
    expect(rendered).toContain('Authorization: Bearer [REDACTED_TOKEN]')
    expect(rendered).toContain('CENTAUR_API_KEY=[REDACTED_TOKEN]')
    expect(rendered).toContain('SLACK_BOT_TOKEN=[REDACTED_TOKEN]')
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
    expect(events).toContainEqual(
      expect.objectContaining({
        type: 'renderer.message.delta',
        delta: 'Execution failed: sandbox exited'
      })
    )
    expect(events.at(-1)).toMatchObject({
      type: 'renderer.done',
      answerMarkdown: 'Execution failed: sandbox exited',
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
