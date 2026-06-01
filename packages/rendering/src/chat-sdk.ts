import type { RendererEvent, RendererTaskBlock, RendererTaskStatus, RendererTaskUpdate } from './types'
import type { RendererInterface } from './interface'

export type ChatSDKStreamChunk =
  | { type: 'markdown_text'; text: string }
  | {
      type: 'task_update'
      id: string
      title: string
      status: RendererTaskStatus
      details?: string
      output?: string
    }
  | { type: 'plan_update'; title: string }

export type ChatSDKPostableMessage = {
  text?: string
  title?: string
  error?: string
}

export type ChatSDKStreamAppend = {
  type: 'chat.stream.append'
  chunks: ChatSDKStreamChunk[]
  force?: boolean
  planPrefix?: boolean
}

export type ChatSDKMessageUpsert = {
  type: 'chat.message.upsert'
  message: ChatSDKPostableMessage
}

export type ChatSDKSessionClosed = {
  type: 'chat.session.closed'
  message?: ChatSDKPostableMessage
  streamFinalUpdates?: boolean
}

export type ChatSDKOutput = ChatSDKMessageUpsert | ChatSDKSessionClosed | ChatSDKStreamAppend

export class ChatSDKRenderer implements RendererInterface<ChatSDKOutput> {
  open(): ChatSDKOutput[] {
    return []
  }

  render(_sessionId: string, event: RendererEvent): ChatSDKOutput[] {
    return this.consume(event)
  }

  close(_sessionId: string, event?: Extract<RendererEvent, { type: 'renderer.done' }>): ChatSDKOutput[] {
    return event ? this.consume(event) : []
  }

  consume(event: RendererEvent): ChatSDKOutput[] {
    if (event.type === 'renderer.session.open') {
      return []
    }
    if (event.type === 'renderer.status') {
      return [{ type: 'chat.message.upsert', message: { text: event.status } }]
    }
    if (event.type === 'renderer.message.delta') {
      return [
        {
          type: 'chat.stream.append',
          chunks: [{ type: 'markdown_text', text: event.delta }],
          force: event.force,
          planPrefix: event.planPrefix
        }
      ]
    }
    if (event.type === 'renderer.message.snapshot') {
      return [{ type: 'chat.message.upsert', message: { text: event.markdown } }]
    }
    if (event.type === 'renderer.task.update') {
      return [{ type: 'chat.stream.append', chunks: [taskChunk(event.task)] }]
    }
    if (event.type === 'renderer.plan.update') {
      return [{ type: 'chat.stream.append', chunks: [{ type: 'plan_update', title: event.title }] }]
    }
    if (event.type === 'renderer.title.update') {
      return [{ type: 'chat.message.upsert', message: { title: event.title } }]
    }
    return [
      {
        type: 'chat.session.closed',
        message: {
          text: event.answerMarkdown,
          error: event.error
        },
        streamFinalUpdates: event.streamFinalUpdates
      }
    ]
  }
}

function taskChunk(task: RendererTaskUpdate): ChatSDKStreamChunk {
  return {
    type: 'task_update',
    id: task.id,
    title: task.title,
    status: task.status,
    ...(task.details?.length ? { details: taskBodyToChatSdkText(task.details) } : {}),
    ...(task.output?.length ? { output: taskBodyToChatSdkText(task.output) } : {})
  }
}

function taskBodyToChatSdkText(blocks: RendererTaskBlock[]): string {
  return blocks
    .map(block => {
      if (block.type === 'text') return block.text
      const language = block.language ?? ''
      return `\`\`\`${language}\n${block.text}\n\`\`\``
    })
    .filter(Boolean)
    .join('\n')
}
