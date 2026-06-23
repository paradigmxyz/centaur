import type { RendererEvent, RendererSessionOpenInput } from './types'

export type RendererSession = {
  sessionId: string
}

export interface RendererInterface<TOutput = unknown> {
  open(input: RendererSessionOpenInput): TOutput[] | Promise<TOutput[]>
  render(sessionId: string, event: RendererEvent): TOutput[] | Promise<TOutput[]>
  close(sessionId: string, event?: Extract<RendererEvent, { type: 'renderer.done' }>): TOutput[] | Promise<TOutput[]>
}
