import type { JsonObject, JsonValue, StoredConversationReference, TeamsActivity } from './types.js';

export function toStoredConversationReference(activity: TeamsActivity): StoredConversationReference {
  return {
    activityId: activity.id,
    bot: toJsonObject(activity.recipient),
    channelId: activity.channelId,
    conversation: toJsonObject(activity.conversation),
    conversationId: activity.conversation?.id ?? 'unknown-conversation',
    conversationType: activity.conversation?.conversationType,
    serviceUrl: activity.serviceUrl,
    teamId: teamsGetTeamId(activity) ?? activity.channelData?.team?.id,
    tenantId: teamsGetTenantId(activity) ?? activity.conversation?.tenantId,
    user: toJsonObject(activity.from),
  };
}

export function toBotFrameworkConversationReference(
  reference: StoredConversationReference,
): Record<string, unknown> {
  return {
    activityId: reference.activityId,
    bot: reference.bot,
    channelId: reference.channelId,
    conversation: reference.conversation ?? {
      id: reference.conversationId,
      conversationType: reference.conversationType,
      tenantId: reference.tenantId,
    },
    serviceUrl: reference.serviceUrl,
    user: reference.user,
  };
}

function teamsGetTeamId(activity: TeamsActivity): string | undefined {
  return activity.channelData?.teamsTeamId ?? activity.channelData?.team?.id;
}

function teamsGetTenantId(activity: TeamsActivity): string | undefined {
  return activity.channelData?.tenant?.id;
}

function toJsonObject(value: unknown): JsonObject | undefined {
  const json = toJsonValue(value);
  return typeof json === 'object' && json !== null && !Array.isArray(json) ? json : undefined;
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
