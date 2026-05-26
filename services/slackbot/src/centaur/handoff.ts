import { centaurApiKey, type AppConfig } from '../config'
import type { NormalizedSlackEvent } from '../slack/types'

export type CentaurHandoffResult =
  | { ok: true; status: number; body: unknown }
  | { ok: false; status: number; body: unknown }

export class CentaurHandoff {
  readonly config: AppConfig

  constructor(config: AppConfig) {
    this.config = config
  }

  async emit(event: NormalizedSlackEvent): Promise<CentaurHandoffResult> {
    const url = new URL('/workflows/runs', this.config.CENTAUR_API_URL)
    const apiKey = centaurApiKey(this.config)
    const response = await fetch(url, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {})
      },
      body: JSON.stringify({
        workflow_name: 'slack_thread_turn',
        trigger_key: event.message_id,
        input: {
          thread_key: event.thread_key,
          parts: event.parts,
          history_messages: event.history_messages ?? [],
          message_id: event.message_id,
          user_id: event.user_id,
          metadata: {
            source: 'slackbot',
            slack: {
              message_ts: event.slack.message_ts,
              enterprise_id: event.slack.enterprise_id,
              user_team: event.slack.user_team,
              source_team: event.slack.source_team,
              bot_id: event.slack.bot_id,
              app_id: event.slack.app_id,
              bot_user_id: event.slack.bot_user_id
            },
            is_mention: event.is_mention
          },
          delivery: {
            platform: 'slack',
            channel: event.channel_id,
            thread_ts: event.thread_ts,
            recipient_user_id: event.user_id,
            recipient_team_id: event.recipient_team_id ?? event.team_id
          }
        }
      })
    })

    const body = await readResponseBody(response)
    return { ok: response.ok, status: response.status, body }
  }
}

async function readResponseBody(response: Response): Promise<unknown> {
  const text = await response.text()
  if (!text) return null
  try {
    return JSON.parse(text)
  } catch {
    return text
  }
}
