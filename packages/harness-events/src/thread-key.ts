export function splitThreadKey(threadKey: string): { channel: string; threadTs: string } {
  const parts = threadKey.trim().split(":");
  if (parts.length === 2 && parts[0] && parts[1]) {
    return { channel: parts[0], threadTs: parts[1] };
  }
  // Modern Slack keys are `slack:TEAM:CHANNEL:TS` (4 parts).
  if (parts[0] === "slack" && parts.length === 4 && parts[2] && parts[3]) {
    return { channel: parts[2], threadTs: parts[3] };
  }
  if (parts.length === 3 && parts[1] && parts[2]) {
    return { channel: parts[1], threadTs: parts[2] };
  }
  throw new Error(`Invalid thread key format (expected <channel>:<thread_ts>): ${threadKey}`);
}

export function normalizeThreadKey(threadKey: string): string {
  const { channel, threadTs } = splitThreadKey(threadKey);
  return `${channel}:${threadTs}`;
}
