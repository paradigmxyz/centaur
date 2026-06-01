import type { AnyBlock, AnyChunk } from '@slack/types'
import type { WebClient } from '@slack/web-api'
import {
  ChatSDKRenderer,
  type ChatSDKOutput,
  type ChatSDKStreamChunk,
  type RendererEvent,
  type RendererSessionOpenInput
} from '@centaur/rendering'
import { AgentSessionRenderer } from '../slack/agent-session'
import { markdownToStreamChunks } from '../slack/render'

export type SlackRendererOpenInput = RendererSessionOpenInput & {
  target: {
    channel: string
    parentTs: string
    recipientTeamId: string
    recipientUserId: string
  }
  header?: string
}

export type SlackRenderResult = {
  closed?: boolean
  sessionId?: string
  streamedTextChars?: number
}

export type SlackStreamChunk = ChatSDKStreamChunk | AnyChunk

export type SlackStreamOpenInput = {
  target: {
    channel: string
    threadTs: string
    recipientTeamId?: string
    recipientUserId?: string
  }
  markdown?: string
  chunks?: SlackStreamChunk[]
  taskDisplayMode?: 'plan' | 'timeline'
}

export type SlackStreamAppendInput = {
  target: {
    channel: string
    ts: string
  }
  markdown?: string
  chunks?: SlackStreamChunk[]
}

export type SlackStreamStopInput = SlackStreamAppendInput & {
  blocks?: AnyBlock[]
}

export type SlackStreamResult = {
  channel?: string
  ts?: string
}

export class SlackChatSDKRenderer {
  private readonly chatSdk = new ChatSDKRenderer()
  private readonly agentRenderer: AgentSessionRenderer

  constructor(client: WebClient) {
    this.agentRenderer = new AgentSessionRenderer(client)
  }

  async open(input: SlackRendererOpenInput): Promise<SlackRenderResult> {
    validateSlackTarget(input.target)
    const { sessionId } = await this.agentRenderer.open({
      channel: input.target.channel,
      parentTs: input.target.parentTs,
      recipientTeamId: input.target.recipientTeamId,
      recipientUserId: input.target.recipientUserId,
      title: input.title,
      header: input.header
    })
    return { sessionId }
  }

  async render(sessionId: string, event: RendererEvent): Promise<SlackRenderResult> {
    return this.deliver(sessionId, this.chatSdk.render(sessionId, event))
  }

  async close(
    sessionId: string,
    event?: Extract<RendererEvent, { type: 'renderer.done' }>
  ): Promise<SlackRenderResult> {
    return this.deliver(sessionId, this.chatSdk.close(sessionId, event))
  }

  private async deliver(sessionId: string, outputs: ChatSDKOutput[]): Promise<SlackRenderResult> {
    const result: SlackRenderResult = {}
    for (const output of outputs) {
      if (output.type === 'chat.stream.append') {
        for (const chunk of output.chunks) {
          const streamed = await this.deliverStreamChunk(sessionId, chunk, output)
          if (streamed !== undefined) result.streamedTextChars = streamed
        }
      } else if (output.type === 'chat.message.upsert') {
        if (output.message.title) {
          await this.agentRenderer.title(sessionId, output.message.title)
        } else if (output.message.text) {
          await this.agentRenderer.textDelta(sessionId, output.message.text, { force: true })
          result.streamedTextChars = this.agentRenderer.streamedTextChars(sessionId)
        }
      } else if (output.type === 'chat.session.closed') {
        const { streamedTextChars } = await this.agentRenderer.done(sessionId, {
          streamFinalUpdates: output.streamFinalUpdates ?? true,
          answerMarkdown: output.message?.text
        })
        result.closed = true
        result.streamedTextChars = streamedTextChars
      }
    }
    return result
  }

  private async deliverStreamChunk(
    sessionId: string,
    chunk: ChatSDKStreamChunk,
    output: Extract<ChatSDKOutput, { type: 'chat.stream.append' }>
  ): Promise<number | undefined> {
    if (chunk.type === 'markdown_text') {
      await this.agentRenderer.textDelta(sessionId, chunk.text, {
        force: output.force ?? false,
        planPrefix: output.planPrefix
      })
      return this.agentRenderer.streamedTextChars(sessionId)
    }
    if (chunk.type === 'task_update') {
      await this.agentRenderer.step(
        sessionId,
        {
          id: chunk.id,
          title: chunk.title,
          status: chunk.status as any,
          details: chunk.details,
          output: chunk.output
        },
        { flush: true }
      )
    }
    return undefined
  }
}

export class SlackStreamRenderer {
  private readonly chatSdk = new ChatSDKRenderer()

  constructor(private readonly client: WebClient) {}

  async start(input: SlackStreamOpenInput): Promise<SlackStreamResult> {
    const response = assertSlackOk(
      'chat.startStream',
      (await this.client.chat.startStream({
        channel: input.target.channel,
        thread_ts: input.target.threadTs,
        chunks: this.streamChunks(input.markdown ?? ' ', input.chunks),
        recipient_team_id: input.target.recipientTeamId,
        recipient_user_id: input.target.recipientUserId,
        task_display_mode: input.taskDisplayMode
      })) as SlackStreamApiResponse
    )
    return { channel: response.channel, ts: response.ts }
  }

  async append(input: SlackStreamAppendInput): Promise<SlackStreamResult> {
    const response = assertSlackOk(
      'chat.appendStream',
      (await this.client.chat.appendStream({
        channel: input.target.channel,
        ts: input.target.ts,
        chunks: this.streamChunks(input.markdown ?? ' ', input.chunks)
      })) as SlackStreamApiResponse
    )
    return { channel: response.channel, ts: response.ts }
  }

  async stop(input: SlackStreamStopInput): Promise<SlackStreamResult> {
    const response = assertSlackOk(
      'chat.stopStream',
      (await this.client.chat.stopStream({
        channel: input.target.channel,
        ts: input.target.ts,
        chunks: this.optionalStreamChunks(input.markdown, input.chunks),
        blocks: input.blocks
      })) as SlackStreamApiResponse
    )
    return { channel: response.channel, ts: response.ts }
  }

  private streamChunks(markdown: string, chunks?: SlackStreamChunk[]): AnyChunk[] {
    return slackChunksFromChatSdk(chunks ?? this.rendererMarkdownChunks(markdown))
  }

  private optionalStreamChunks(
    markdown?: string,
    chunks?: SlackStreamChunk[]
  ): AnyChunk[] | undefined {
    if (chunks) return slackChunksFromChatSdk(chunks)
    if (markdown) return slackChunksFromChatSdk(this.rendererMarkdownChunks(markdown))
    return undefined
  }

  private rendererMarkdownChunks(markdown: string): ChatSDKStreamChunk[] {
    return this.chatSdk
      .render('legacy-slack-stream', {
        type: 'renderer.message.delta',
        delta: markdown
      })
      .flatMap(output => (output.type === 'chat.stream.append' ? output.chunks : []))
  }
}

function validateSlackTarget(target: SlackRendererOpenInput['target']): void {
  if (!target.channel || !target.parentTs || !target.recipientTeamId || !target.recipientUserId) {
    throw new Error('missing_slack_renderer_target')
  }
}

function slackChunksFromChatSdk(chunks: SlackStreamChunk[]): AnyChunk[] {
  return chunks.flatMap(chunk =>
    isMarkdownTextChunk(chunk) ? markdownToStreamChunks(chunk.text) : [chunk as AnyChunk]
  )
}

function isMarkdownTextChunk(
  chunk: SlackStreamChunk
): chunk is Extract<ChatSDKStreamChunk, { type: 'markdown_text' }> {
  return (
    Boolean(chunk) &&
    typeof chunk === 'object' &&
    'type' in chunk &&
    chunk.type === 'markdown_text' &&
    'text' in chunk &&
    typeof chunk.text === 'string'
  )
}

type SlackStreamApiResponse = {
  ok?: boolean
  error?: string
  channel?: string
  ts?: string
}

function assertSlackOk(method: string, response: SlackStreamApiResponse): SlackStreamApiResponse {
  if (response.ok) return response
  const error = new Error(response.error ?? `${method} failed`) as Error & { data?: unknown }
  error.data = response
  throw error
}
