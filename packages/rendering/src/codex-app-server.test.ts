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
      status: 'complete',
      details: 'Inspecting the event stream'
    })
    expect(chunks).toContainEqual({ type: 'markdown_text', text: 'Done.' })
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
        details: [
          {
            type: 'code',
            language: 'sh',
            text: 'kubectl get pods'
          }
        ],
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
