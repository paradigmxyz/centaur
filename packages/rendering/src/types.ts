export type RendererTaskStatus = 'pending' | 'in_progress' | 'complete' | 'error'

export type RendererTaskBlock =
  | { type: 'text'; text: string }
  | { type: 'code'; text: string; language?: string }

export type RendererTaskBody = RendererTaskBlock[]

export type RendererTask = {
  id: string
  title: string
  status: RendererTaskStatus
  details?: RendererTaskBody
  output?: RendererTaskBody
}

export type RendererTaskUpdate = RendererTask

export type RendererSessionOpenInput = {
  title?: string
  target?: Record<string, unknown>
  metadata?: Record<string, unknown>
}

export type RendererEvent =
  | {
      type: 'renderer.session.open'
      input?: RendererSessionOpenInput
    }
  | {
      type: 'renderer.status'
      status: string
    }
  | {
      type: 'renderer.message.delta'
      delta: string
      force?: boolean
      planPrefix?: boolean
    }
  | {
      type: 'renderer.message.snapshot'
      markdown: string
    }
  | {
      type: 'renderer.task.update'
      task: RendererTaskUpdate
      flush?: boolean
    }
  | {
      type: 'renderer.plan.update'
      title: string
    }
  | {
      type: 'renderer.title.update'
      title: string
    }
  | {
      type: 'renderer.done'
      answerMarkdown?: string
      error?: string
      streamFinalUpdates?: boolean
      threadId?: string
    }

export interface RendererSourceMapper<TSource> {
  process(source: TSource): RendererEvent[]
  flush(): RendererEvent[]
}

export type RendererLogInfo = (event: string, fields: Record<string, unknown>) => void
