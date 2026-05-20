export const slackReplyLimits = {
  text: {
    /** Slack-recommended fallback size when posting with blocks. */
    maxFallbackChars: 4_000,
    /** Hard truncation threshold for plain-text-only posts. */
    maxUntruncatedChars: 40_000
  },
  stream: {
    /** {@link https://docs.slack.dev/reference/methods/chat.postMessage/} */
    markdownChunkChars: 12_000,
    planTitleChars: 256,
    taskCount: 24,
    taskTitleChars: 128,
    /** Slack caps task_update chunk text at 256 chars; keep 10% headroom. */
    taskDetailsChars: 230,
    taskOutputChars: 230,
    /** Keep live accumulated markdown below Slack message-size failures. */
    maxLiveTextChars: 30_000
  },
  finalPlan: {
    maxPayloadBytes: 240_000,
    maxTasks: 24,
    taskTitleChars: 140,
    taskDetailsCodeBlockLines: 4,
    taskOutputCodeBlockLines: 4,
    jsonPreviewChars: 420,
    outputPreviewChars: 2_200,
    taskDetailsCodeBlockChars: 12_000,
    taskOutputCodeBlockChars: 12_000,
    singleTaskCodeBlockChars: 253_000
  },
  mixedBodyAndPlan: {
    maxPayloadBytes: 13_000,
    maxVisibleChars: 6_250
  },
  message: {
    maxBlocks: 50,
    /** Max mrkdwn chars for the thinking context block on finalized replies. */
    thinkingContextChars: 2_800
  }
} as const
