export const slackReplyLimits = {
  text: {
    maxUntruncatedChars: 40_000
  },
  stream: {
    markdownChunkChars: 12_000,
    planTitleChars: 256,
    taskCount: 12,
    taskTitleChars: 128,
    taskDetailsChars: 128,
    taskOutputChars: 48
  },
  finalPlan: {
    maxPayloadBytes: 240_000,
    maxTasks: 12,
    taskTitleChars: 140,
    taskDetailsCodeBlockLines: 3,
    taskOutputCodeBlockLines: 3,
    jsonPreviewChars: 420,
    outputPreviewChars: 2_200,
    taskDetailsCodeBlockChars: 12_000,
    taskOutputCodeBlockChars: 12_000,
    singleTaskCodeBlockChars: 253_000
  },
  mixedBodyAndPlan: {
    maxPayloadBytes: 13_000,
    maxVisibleChars: 6_250
  }
} as const
