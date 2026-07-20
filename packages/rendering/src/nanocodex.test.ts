import { describe, expect, test } from 'bun:test'
import { NanocodexRendererEventMapper, isNanocodexEvent } from './nanocodex'

const native = (type: string, payload: Record<string, unknown>, seq = 1) => ({
  eventKind: 'session.output.line',
  data: JSON.stringify({
    protocol_version: 1,
    request_id: 'nano-session',
    seq,
    type,
    payload
  })
})

describe('NanocodexRendererEventMapper', () => {
  test('streams native assistant events and completes with the canonical answer', () => {
    const mapper = new NanocodexRendererEventMapper()
    expect(isNanocodexEvent(native('run.started', {}))).toBe(true)
    expect(mapper.process(native('assistant.delta', { text: 'hello' }, 2))).toEqual([
      { type: 'renderer.message.delta', delta: 'hello' }
    ])
    expect(mapper.process(native('assistant.message', { text: 'hello world' }, 3))).toEqual([
      { type: 'renderer.message.delta', delta: ' world' }
    ])
    expect(mapper.process(native('run.completed', {}, 4))).toEqual([
      {
        type: 'renderer.done',
        answerMarkdown: 'hello world',
        streamFinalUpdates: true,
        threadId: 'nano-session'
      }
    ])
  })

  test('suppresses commentary and streams only the final-answer item', () => {
    const mapper = new NanocodexRendererEventMapper()
    expect(
      mapper.process(
        native('assistant.delta', {
          item_id: 'commentary-1',
          phase: 'commentary',
          text: 'I’ll verify.'
        })
      )
    ).toEqual([])
    expect(
      mapper.process(
        native('assistant.delta', {
          item_id: 'answer-1',
          phase: 'final_answer',
          text: 'Done.'
        }, 2)
      )
    ).toEqual([{ type: 'renderer.message.delta', delta: 'Done.' }])
    expect(
      mapper.process(
        native('assistant.message', {
          phase: 'final_answer',
          text: 'Done.'
        }, 3)
      )
    ).toEqual([])
    expect(mapper.process(native('run.completed', {}, 4))).toEqual([
      {
        type: 'renderer.done',
        answerMarkdown: 'Done.',
        streamFinalUpdates: true,
        threadId: 'nano-session'
      }
    ])
  })

  test('renders native tool lifecycle without an app-server conversion', () => {
    const mapper = new NanocodexRendererEventMapper()
    expect(
      mapper.process(
        native('tool.call', { call_id: 'call-1', tool: 'shell', arguments: { cmd: 'pwd' } })
      )[0]
    ).toMatchObject({
      type: 'renderer.task.update',
      task: { id: 'call-1', title: 'shell', status: 'in_progress' }
    })
    expect(
      mapper.process(
        native('tool.result', { call_id: 'call-1', tool: 'shell', status: 'completed', result: 'ok' })
      )[0]
    ).toMatchObject({
      type: 'renderer.task.update',
      task: { id: 'call-1', title: 'shell', status: 'complete' }
    })
  })

  test('carries run.error into the terminal failure', () => {
    const mapper = new NanocodexRendererEventMapper()
    expect(mapper.process(native('run.error', { message: 'proxy refused' }))).toEqual([])
    expect(mapper.process(native('run.failed', {}, 2))).toEqual([
      {
        type: 'renderer.done',
        answerMarkdown: undefined,
        error: 'proxy refused',
        streamFinalUpdates: true,
        threadId: 'nano-session'
      }
    ])
  })

  test('renders a cancelled run through the stock interruption path', () => {
    const mapper = new NanocodexRendererEventMapper()
    expect(mapper.process(native('run.error', { message: 'turn cancelled' }))).toEqual([])
    expect(mapper.process(native('run.failed', { status: 'cancelled' }, 2))).toEqual([
      {
        type: 'renderer.done',
        answerMarkdown: 'Execution interrupted',
        streamFinalUpdates: true,
        threadId: 'nano-session'
      }
    ])
  })
})
