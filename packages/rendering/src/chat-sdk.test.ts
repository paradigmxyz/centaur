import { describe, expect, it } from 'bun:test'
import { ChatSDKRenderer } from './chat-sdk'
import { rendererEventTypes } from './schema'
import type { RendererInterface } from './interface'

describe('ChatSDKRenderer', () => {
  it('implements the generic renderer interface over renderer events', () => {
    const renderer: RendererInterface = new ChatSDKRenderer()

    expect(renderer.open({ title: 'Execution' })).toEqual([])
    expect(
      renderer.render('session-1', {
        type: 'renderer.message.delta',
        delta: 'hello'
      })
    ).toEqual([
      {
        type: 'chat.stream.append',
        chunks: [{ type: 'markdown_text', text: 'hello' }],
        force: undefined,
        planPrefix: undefined
      }
    ])
    expect(
      renderer.close('session-1', {
        type: 'renderer.done',
        answerMarkdown: 'done'
      })
    ).toEqual([
      {
        type: 'chat.session.closed',
        message: { text: 'done', error: undefined },
        streamFinalUpdates: undefined
      }
    ])
  })

  it('exposes the renderer contract event names', () => {
    expect(rendererEventTypes).toContain('renderer.session.open')
    expect(rendererEventTypes).toContain('renderer.status')
    expect(rendererEventTypes).toContain('renderer.message.delta')
    expect(rendererEventTypes).toContain('renderer.message.snapshot')
    expect(rendererEventTypes).toContain('renderer.task.update')
    expect(rendererEventTypes).toContain('renderer.plan.update')
    expect(rendererEventTypes).toContain('renderer.done')
  })

  it('maps generic plan updates to Chat SDK plan chunks', () => {
    const renderer = new ChatSDKRenderer()

    expect(
      renderer.render('session-1', {
        type: 'renderer.plan.update',
        title: 'Implementation plan'
      })
    ).toEqual([
      {
        type: 'chat.stream.append',
        chunks: [{ type: 'plan_update', title: 'Implementation plan' }]
      }
    ])
  })
})
