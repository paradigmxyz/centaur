import type { AnyBlock, AnyChunk, MarkdownBlock, RichTextBlock } from '@slack/types'

export type NormalizedTextPart = {
  type: 'text'
  text: string
}

export type NormalizedBinaryPart = {
  type: 'image' | 'document' | 'file'
  name: string
  mime_type: string
  size: number
  slack_file_id?: string
  source: {
    type: 'base64'
    media_type: string
    data: string
  }
}

export type NormalizedPart = NormalizedTextPart | NormalizedBinaryPart

export type NormalizedSlackEvent = {
  thread_key: string
  message_id: string
  team_id: string
  recipient_team_id?: string
  user_id: string
  channel_id: string
  thread_ts: string
  is_mention: boolean
  parts: NormalizedPart[]
  history_messages?: Array<{
    message_id: string
    role?: 'user' | 'assistant'
    parts: NormalizedPart[]
    user_id?: string
    metadata?: Record<string, unknown>
  }>
  slack: {
    event_id?: string
    event_ts?: string
    message_ts: string
    enterprise_id?: string
    user_team?: string
    source_team?: string
    bot_id?: string
    app_id?: string
    bot_user_id?: string
  }
}

export type SlackEnvelope = {
  token?: string
  challenge?: string
  type?: string
  team_id?: string
  enterprise_id?: string
  event_id?: string
  event_time?: number
  event?: Record<string, unknown>
}

export type SlackMessageFile = {
  id?: string
  name?: string
  title?: string
  mimetype?: string
  filetype?: string
  url_private?: string
  url_private_download?: string
  size?: number
}

export type SlackRenderableBlock = MarkdownBlock | RichTextBlock

export type SlackBlocks = SlackRenderableBlock[] | AnyBlock[]

export type SlackStreamChunk = AnyChunk
