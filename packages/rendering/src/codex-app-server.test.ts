import { describe, expect, it } from 'bun:test'
import { CodexAppServerRendererEventMapper } from './codex-app-server'
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
