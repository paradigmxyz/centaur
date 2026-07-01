import type { JsonValue, TeamsActivity, TeamsApiAttachment, TeamsApiMessage } from './types.js';

type TeamsActivityContext = { activity: TeamsActivity };

const TEAMS_FILE_DOWNLOAD_INFO = 'application/vnd.microsoft.teams.file.download.info';

export function messageMentionsBot(context: TeamsActivityContext): boolean {
  const recipientId = context.activity.recipient?.id;
  if (!recipientId) {
    return false;
  }
  return getMentions(context.activity).some((mention) => {
    return mention.mentioned?.id?.toLowerCase() === recipientId.toLowerCase();
  });
}

export function normalizeTeamsText(context: TeamsActivityContext): string {
  return removeRecipientMention(context.activity).trim();
}

export function serializeTeamsMessage(
  context: TeamsActivityContext,
  threadId: string,
  text = normalizeTeamsText(context),
): TeamsApiMessage {
  const activity = context.activity;
  return {
    attachments: serializeAttachments(activity.attachments ?? []),
    author: {
      aadObjectId: activity.from?.aadObjectId,
      fullName: activity.from?.name,
      isBot: activity.from?.id?.toLowerCase() === activity.recipient?.id?.toLowerCase(),
      userId: activity.from?.id ?? 'unknown-user',
      userName: activity.from?.name,
    },
    channelId: teamsGetChannelId(activity) ?? activity.channelData?.channel?.id,
    conversationId: activity.conversation?.id ?? 'unknown-conversation',
    conversationType: activity.conversation?.conversationType,
    id: activity.id ?? activity.replyToId ?? `${Date.now()}`,
    isMention: messageMentionsBot(context),
    raw: toJsonValue(redactTeamsAttachmentUrls(activity)),
    teamId: teamsGetTeamId(activity) ?? activity.channelData?.team?.id,
    tenantId: teamsGetTenantId(activity) ?? activity.conversation?.tenantId,
    text,
    threadId,
    timestamp: activity.timestamp ? new Date(activity.timestamp).toISOString() : new Date().toISOString(),
  };
}

export function teamsQuotedReplyContextMessages(activity: TeamsActivity, threadId: string): TeamsApiMessage[] {
  const quotedReplies = getQuotedReplies(activity);
  return quotedReplies
    .filter((entity) => entity.quotedReply && !entity.quotedReply.isReplyDeleted)
    .map((entity, index) => {
      const quoted = entity.quotedReply!;
      const messageId = quoted.messageId || `quoted-${index + 1}`;
      return {
        attachments: [],
        author: {
          fullName: quoted.senderName ?? undefined,
          isBot: false,
          userId: quoted.senderId || 'unknown-user',
          userName: quoted.senderName ?? undefined,
        },
        channelId: teamsGetChannelId(activity) ?? activity.channelData?.channel?.id,
        conversationId: activity.conversation?.id ?? 'unknown-conversation',
        conversationType: activity.conversation?.conversationType,
        id: messageId,
        isMention: false,
        raw: toJsonValue(entity),
        teamId: teamsGetTeamId(activity) ?? activity.channelData?.team?.id,
        tenantId: teamsGetTenantId(activity) ?? activity.conversation?.tenantId,
        text: quoted.preview ?? '',
        threadId,
        timestamp: quoted.time ? quotedTimestamp(quoted.time) : new Date(0).toISOString(),
      };
    });
}

function serializeAttachments(attachments: NonNullable<TeamsActivity['attachments']>): TeamsApiAttachment[] {
  return attachments
    .filter((attachment) => !isTeamsMessageBodyAttachment(attachment))
    .map((attachment, index) => ({
      content: attachment.content === undefined ? undefined : toJsonValue(redactTeamsAttachmentUrls(attachment.content)),
      contentType: attachment.contentType ?? 'unknown',
      contentUrl: attachment.contentUrl,
      name: attachment.name || `attachment-${index + 1}`,
    }));
}

function isTeamsMessageBodyAttachment(attachment: NonNullable<TeamsActivity['attachments']>[number]): boolean {
  if (attachment.contentType?.toLowerCase() !== 'text/html') {
    return false;
  }
  return typeof attachment.content === 'string' && !attachment.contentUrl;
}

export function isAllowedTeamsActivity(input: {
  activity: TeamsActivity;
  allowedChannelIds: readonly string[];
  allowedTeamIds: readonly string[];
  allowedTenantIds: readonly string[];
}): boolean {
  if (input.activity.channelId !== 'msteams') {
    return false;
  }
  if (
    input.allowedTeamIds.length === 0
    && input.allowedChannelIds.length === 0
    && input.allowedTenantIds.length === 0
  ) {
    return false;
  }
  const tenantId = teamsGetTenantId(input.activity) ?? input.activity.conversation?.tenantId;
  if (input.allowedTenantIds.length > 0 && (!tenantId || !input.allowedTenantIds.includes(tenantId))) {
    return false;
  }
  if (input.activity.conversation?.conversationType?.toLowerCase() === 'personal') {
    return Boolean(tenantId && input.allowedTenantIds.includes(tenantId));
  }
  const teamId = teamsGetTeamId(input.activity);
  const channelId = teamsGetChannelId(input.activity);
  if (input.allowedTeamIds.length === 0 && input.allowedChannelIds.length === 0) {
    return false;
  }
  if (input.allowedTeamIds.length > 0 && (!teamId || !input.allowedTeamIds.includes(teamId))) {
    return false;
  }
  if (input.allowedChannelIds.length > 0 && (!channelId || !input.allowedChannelIds.includes(channelId))) {
    return false;
  }
  return true;
}

function getMentions(activity: TeamsActivity): NonNullable<TeamsActivity['entities']> {
  return (activity.entities ?? []).filter((entity) => entity.type?.toLowerCase() === 'mention');
}

function getQuotedReplies(activity: TeamsActivity): NonNullable<TeamsActivity['entities']> {
  return (activity.entities ?? []).filter((entity) => entity.type?.toLowerCase() === 'quotedreply');
}

function removeRecipientMention(activity: TeamsActivity): string {
  let text = String(activity.text ?? '');
  const recipientId = activity.recipient?.id?.toLowerCase();
  for (const mention of getMentions(activity)) {
    if (!mention.text) {
      continue;
    }
    if (!recipientId || mention.mentioned?.id?.toLowerCase() === recipientId) {
      text = text.replaceAll(mention.text, '');
    }
  }
  return text;
}

function teamsGetChannelId(activity: TeamsActivity): string | undefined {
  return activity.channelData?.teamsChannelId ?? activity.channelData?.channel?.id;
}

function teamsGetTeamId(activity: TeamsActivity): string | undefined {
  return activity.channelData?.teamsTeamId ?? activity.channelData?.team?.id;
}

function teamsGetTenantId(activity: TeamsActivity): string | undefined {
  return activity.channelData?.tenant?.id;
}

function redactTeamsAttachmentUrls<T>(value: T): T {
  if (!isRecord(value)) {
    return value;
  }
  if (isTeamsAttachmentObject(value)) {
    const redacted: Record<string, unknown> = { ...value };
    delete redacted.contentUrl;
    redacted.contentUrlRedacted = true;
    if (isTeamsFileDownloadInfo(redacted)) {
      delete redacted.downloadUrl;
      redacted.downloadUrlRedacted = true;
    }
    return mapRecord(redacted, redactTeamsAttachmentUrls) as T;
  }
  if (isTeamsFileDownloadInfo(value)) {
    const redacted: Record<string, unknown> = { ...value };
    delete redacted.downloadUrl;
    redacted.downloadUrlRedacted = true;
    return redacted as T;
  }
  if (Array.isArray(value)) {
    return value.map(redactTeamsAttachmentUrls) as T;
  }
  return mapRecord(value, redactTeamsAttachmentUrls) as T;
}

function isTeamsAttachmentObject(value: Record<string, unknown>): boolean {
  return typeof value.contentUrl === 'string' && typeof value.contentType === 'string';
}

function isTeamsFileDownloadInfo(value: Record<string, unknown>): boolean {
  return typeof value.downloadUrl === 'string'
    && (
      value.contentType === TEAMS_FILE_DOWNLOAD_INFO
      || typeof value.fileName === 'string'
      || typeof value.fileType === 'string'
    );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function mapRecord(
  value: Record<string, unknown>,
  mapper: (entry: unknown) => unknown,
): Record<string, unknown> {
  const output: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value)) {
    output[key] = mapper(entry);
  }
  return output;
}

function quotedTimestamp(value: string): string {
  const epochMs = Number(value);
  if (Number.isFinite(epochMs) && epochMs > 0) {
    return new Date(epochMs).toISOString();
  }
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? new Date(0).toISOString() : parsed.toISOString();
}

function toJsonValue(value: unknown): JsonValue {
  if (value === null || typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map(toJsonValue);
  }
  if (typeof value === 'object' && value !== null) {
    const output: Record<string, JsonValue> = {};
    for (const [key, entry] of Object.entries(value)) {
      if (entry !== undefined && typeof entry !== 'function') {
        output[key] = toJsonValue(entry);
      }
    }
    return output;
  }
  return String(value);
}
