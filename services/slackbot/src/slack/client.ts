import type { AnyBlock, AnyChunk } from '@slack/types'
import type {
  ChatAppendStreamResponse,
  ChatPostMessageResponse,
  ChatStartStreamResponse,
  ChatStopStreamResponse,
  ChatUpdateResponse,
  FilesCompleteUploadExternalResponse,
  WebAPICallResult
} from '@slack/web-api'
import { WebClient } from '@slack/web-api'
import {
  enforceBlockLimits,
  fallbackText,
  markdownToStreamChunks,
  renderMarkdownBlocks,
  renderStatusBlock,
  type StatusMetadata
} from './render'

type SlackCallResult = WebAPICallResult & { ok?: boolean; error?: string }

export class SlackApiError extends Error {
  readonly response: SlackCallResult

  constructor(method: string, response: SlackCallResult) {
    super(`Slack ${method} failed: ${response.error ?? 'unknown_error'}`)
    this.name = 'SlackApiError'
    this.response = response
  }
}

export class SlackEdgeClient {
  readonly client: WebClient

  constructor(client: WebClient) {
    this.client = client
  }

  async post(opts: {
    channel: string
    threadTs?: string
    markdown: string
    metadata?: StatusMetadata
    fallback?: string
  }): Promise<ChatPostMessageResponse> {
    const blocks = this.renderBlocks(opts.markdown, opts.metadata)
    const response = await this.client.chat.postMessage({
      channel: opts.channel,
      thread_ts: opts.threadTs,
      text: fallbackText(opts),
      blocks
    })
    return assertSlackOk('chat.postMessage', response)
  }

  async update(opts: {
    channel: string
    ts: string
    markdown: string
    metadata?: StatusMetadata
    fallback?: string
  }): Promise<ChatUpdateResponse> {
    const blocks = this.renderBlocks(opts.markdown, opts.metadata)
    const response = await this.client.chat.update({
      channel: opts.channel,
      ts: opts.ts,
      text: fallbackText(opts),
      blocks
    })
    return assertSlackOk('chat.update', response)
  }

  async startStream(opts: {
    channel: string
    threadTs: string
    markdown?: string
    chunks?: AnyChunk[]
    recipientTeamId?: string
    recipientUserId?: string
    taskDisplayMode?: 'plan' | 'timeline' | 'dense'
  }): Promise<ChatStartStreamResponse> {
    const response = await this.client.chat.startStream({
      channel: opts.channel,
      thread_ts: opts.threadTs,
      recipient_team_id: opts.recipientTeamId,
      recipient_user_id: opts.recipientUserId,
      task_display_mode: opts.taskDisplayMode,
      chunks: opts.chunks ?? markdownToStreamChunks(opts.markdown ?? ' ')
    })
    return assertSlackOk('chat.startStream', response)
  }

  async appendStream(opts: {
    channel: string
    ts: string
    markdown?: string
    chunks?: AnyChunk[]
  }): Promise<ChatAppendStreamResponse> {
    const response = await this.client.chat.appendStream({
      channel: opts.channel,
      ts: opts.ts,
      chunks: opts.chunks ?? markdownToStreamChunks(opts.markdown ?? ' ')
    })
    return assertSlackOk('chat.appendStream', response)
  }

  async stopStream(opts: {
    channel: string
    ts: string
    markdown?: string
    chunks?: AnyChunk[]
    blocks?: AnyBlock[]
  }): Promise<ChatStopStreamResponse> {
    const response = await this.client.chat.stopStream({
      channel: opts.channel,
      ts: opts.ts,
      chunks: opts.chunks ?? (opts.markdown ? markdownToStreamChunks(opts.markdown) : undefined),
      blocks: opts.blocks ? enforceBlockLimits(opts.blocks) : undefined
    })
    return assertSlackOk('chat.stopStream', response)
  }

  async setStatus(opts: {
    channelId: string
    threadTs: string
    status: string
    loadingMessages?: string[]
  }): Promise<WebAPICallResult> {
    const response = await this.client.assistant.threads.setStatus({
      channel_id: opts.channelId,
      thread_ts: opts.threadTs,
      status: opts.status,
      loading_messages: opts.loadingMessages
    })
    return assertSlackOk('assistant.threads.setStatus', response)
  }

  async setTitle(opts: {
    channelId: string
    threadTs: string
    title: string
  }): Promise<WebAPICallResult> {
    const response = await this.client.assistant.threads.setTitle({
      channel_id: opts.channelId,
      thread_ts: opts.threadTs,
      title: opts.title
    })
    return assertSlackOk('assistant.threads.setTitle', response)
  }

  async uploadFile(opts: {
    channelId: string
    threadTs?: string
    filename: string
    bytes: Uint8Array
    title?: string
    altText?: string
    blocks?: AnyBlock[]
  }): Promise<FilesCompleteUploadExternalResponse> {
    const upload = await this.client.files.getUploadURLExternal({
      filename: opts.filename,
      length: opts.bytes.byteLength,
      alt_text: opts.altText
    })
    const uploadResult = assertSlackOk('files.getUploadURLExternal', upload)
    if (!uploadResult.upload_url || !uploadResult.file_id) {
      throw new SlackApiError('files.getUploadURLExternal', {
        ...uploadResult,
        ok: false,
        error: 'missing_upload_url_or_file_id'
      })
    }

    const uploadResponse = await fetch(uploadResult.upload_url, {
      method: 'POST',
      body: Buffer.from(opts.bytes)
    })
    if (!uploadResponse.ok) {
      throw new Error(`Slack external file upload failed: ${uploadResponse.status}`)
    }

    const complete = await this.client.files.completeUploadExternal({
      channel_id: opts.channelId,
      thread_ts: opts.threadTs,
      files: [{ id: uploadResult.file_id, title: opts.title ?? opts.filename }],
      blocks: opts.blocks ? enforceBlockLimits(opts.blocks) : undefined
    })
    return assertSlackOk('files.completeUploadExternal', complete)
  }

  private renderBlocks(markdown: string, metadata?: StatusMetadata): AnyBlock[] {
    const blocks: AnyBlock[] = [...renderMarkdownBlocks(markdown)]
    const status = metadata ? renderStatusBlock(metadata) : null
    if (status) blocks.push(status)
    return enforceBlockLimits(blocks)
  }
}

function assertSlackOk<T extends SlackCallResult>(method: string, response: T): T {
  if (!response.ok) throw new SlackApiError(method, response)
  return response
}
