export type DuplicateSlackEventDetails = {
  dedupe_key: string
  event_id?: string
  team_id?: string
  channel_id?: string
  message_ts?: string
  thread_ts?: string
  event_type?: string
  codex_thread_id?: string
}

export function duplicateSlackAlertText(details: DuplicateSlackEventDetails): string {
  return [
    '*Slack duplicate message skipped*',
    details.channel_id ? `*Channel:* \`${details.channel_id}\`` : '',
    details.thread_ts ? `*Thread:* \`${details.thread_ts}\`` : '',
    details.message_ts ? `*Message:* \`${details.message_ts}\`` : '',
    details.event_type ? `*Event type:* \`${details.event_type}\`` : '',
    details.event_id ? `*Event ID:* \`${details.event_id}\`` : '',
    details.codex_thread_id ? `*Codex thread:* \`${details.codex_thread_id}\`` : '',
    `*Dedupe key:* \`${details.dedupe_key}\``
  ]
    .filter(Boolean)
    .join('\n')
}
