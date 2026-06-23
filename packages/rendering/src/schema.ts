export type {
  RendererEvent,
  RendererSessionOpenInput,
  RendererTask,
  RendererTaskBlock,
  RendererTaskBody,
  RendererTaskStatus,
  RendererTaskUpdate
} from './types'

export const rendererEventTypes = [
  'renderer.session.open',
  'renderer.status',
  'renderer.message.delta',
  'renderer.message.snapshot',
  'renderer.task.update',
  'renderer.plan.update',
  'renderer.title.update',
  'renderer.done'
] as const

export type RendererEventType = (typeof rendererEventTypes)[number]
