import { createSlackbotV2, type SlackbotV2Options } from './index'
import { parseChannelDefaults } from './channel-defaults'
import {
  createFlagMessageOverridesStrategy,
  createOpenAiMessageOverridesStrategy
} from './message-overrides-strategy'

const port = numberEnv('PORT', 3002)
const apiUrl = stringEnv('CENTAUR_API_URL', 'http://127.0.0.1:8080')
const botToken = requiredEnv('SLACK_BOT_TOKEN')
const signingSecret = requiredEnv('SLACK_SIGNING_SECRET')
const messageOverridesStrategyMode = messageOverridesStrategyModeEnv(
  'SLACKBOTV2_MESSAGE_OVERRIDES_STRATEGY'
)
const messageOverridesStrategyApiKey =
  optionalEnv('SLACKBOTV2_MESSAGE_OVERRIDES_OPENAI_API_KEY') ?? optionalEnv('OPENAI_API_KEY')

// Default to info: the chat adapter logs entire raw Slack webhook bodies at
// debug, and JSON-serializing those multi-hundred-KB payloads on the hot path
// blocks the event loop long enough to fail the 1s liveness probe.
const LOG_LEVELS = ['debug', 'info', 'warn', 'error'] as const
const minLogLevel: (typeof LOG_LEVELS)[number] = (() => {
  const value = optionalEnv('SLACKBOTV2_LOG_LEVEL')?.toLowerCase()
  return (LOG_LEVELS as readonly string[]).includes(value ?? '')
    ? (value as (typeof LOG_LEVELS)[number])
    : 'info'
})()

const consoleLogger = {
  debug: (message: string, data?: unknown) => log('debug', message, data),
  info: (message: string, data?: unknown) => log('info', message, data),
  warn: (message: string, data?: unknown) => log('warn', message, data),
  error: (message: string, data?: unknown) => log('error', message, data),
  child: () => consoleLogger
}

const options: SlackbotV2Options = {
  apiUrl,
  apiKey: optionalEnv('SLACKBOT_API_KEY'),
  assistantStatus: optionalEnv('SLACKBOTV2_ASSISTANT_STATUS'),
  activitySummaryStatusEnabled: booleanEnv('SLACKBOTV2_ACTIVITY_SUMMARY_STATUS_ENABLED', false),
  botToken,
  botUserId: optionalEnv('SLACK_BOT_USER_ID'),
  channelDefaults: parseChannelDefaults(optionalEnv('SLACKBOTV2_CHANNEL_DEFAULTS'), reason =>
    consoleLogger.warn('slackbotv2 SLACKBOTV2_CHANNEL_DEFAULTS', { reason })
  ),
  consolePublicUrl: optionalEnv('CENTAUR_CONSOLE_PUBLIC_URL'),
  defaultHarnessType: optionalEnv('SLACKBOTV2_DEFAULT_HARNESS'),
  // Same env vars deployers use to override the sandbox harness model
  // (sandbox.extraEnv); the chart mirrors them here so displayed defaults
  // track the deployment instead of the baked harness config.
  harnessDefaultModels: {
    ...(optionalEnv('CLAUDE_MODEL') ? { claudecode: optionalEnv('CLAUDE_MODEL')! } : {}),
    ...(optionalEnv('CODEX_MODEL') ? { codex: optionalEnv('CODEX_MODEL')! } : {})
  },
  idleTimeoutMs: optionalNumberEnv('SESSION_IDLE_TIMEOUT_MS'),
  maxDurationMs: optionalNumberEnv('SESSION_MAX_DURATION_MS'),
  messageOverridesStrategy: createMessageOverridesStrategy(),
  postgresUrl:
    optionalEnv('SLACKBOTV2_DATABASE_URL') ??
    optionalEnv('DATABASE_URL') ??
    optionalEnv('POSTGRES_URL'),
  renderRecoveryMaxObligationAgeMs: optionalNumberEnv(
    'SLACKBOTV2_RENDER_RECOVERY_MAX_OBLIGATION_AGE_MS'
  ),
  sessionApiTimeoutMs: optionalNumberEnv('SLACKBOTV2_SESSION_API_TIMEOUT_MS'),
  signingSecret,
  slackApiUrl: optionalEnv('SLACK_API_URL'),
  slackApiTimeoutMs: optionalNumberEnv('SLACKBOTV2_SLACK_API_TIMEOUT_MS'),
  stateKeyPrefix: optionalEnv('SLACKBOTV2_STATE_KEY_PREFIX'),
  userName: stringEnv('SLACKBOTV2_USER_NAME', 'centaur'),
  logger: consoleLogger
}

const { app } = createSlackbotV2(options)
const server = Bun.serve({
  port,
  fetch: app.fetch
})

console.log(
  JSON.stringify({
    timestamp: new Date().toISOString(),
    level: 'info',
    event: 'slackbotv2_started',
    service: 'slackbotv2',
    activity_summary_status_enabled: options.activitySummaryStatusEnabled,
    message_overrides_strategy: messageOverridesStrategyMode,
    message_overrides_strategy_enabled:
      messageOverridesStrategyMode !== 'llm' || Boolean(messageOverridesStrategyApiKey),
    port: server.port,
    api_url: apiUrl
  })
)

function optionalEnv(name: string): string | undefined {
  const value = process.env[name]?.trim()
  return value ? value : undefined
}

function requiredEnv(name: string): string {
  const value = optionalEnv(name)
  if (!value) {
    throw new Error(`${name} is required`)
  }
  return value
}

function stringEnv(name: string, fallback: string): string {
  return optionalEnv(name) ?? fallback
}

function numberEnv(name: string, fallback: number): number {
  return optionalNumberEnv(name) ?? fallback
}

function booleanEnv(name: string, fallback: boolean): boolean {
  const value = optionalEnv(name)
  if (!value) return fallback
  if (['1', 'true', 'yes', 'on'].includes(value.toLowerCase())) return true
  if (['0', 'false', 'no', 'off'].includes(value.toLowerCase())) return false
  throw new Error(`${name} must be a boolean`)
}

function messageOverridesStrategyModeEnv(name: string): 'flags' | 'llm' {
  const value = optionalEnv(name)?.toLowerCase()
  if (!value) return 'flags'
  if (value === 'flags' || value === 'llm') return value
  throw new Error(`${name} must be "flags" or "llm"`)
}

function createMessageOverridesStrategy(): SlackbotV2Options['messageOverridesStrategy'] {
  if (messageOverridesStrategyMode !== 'llm') return createFlagMessageOverridesStrategy()
  if (!messageOverridesStrategyApiKey) {
    return async () => ({ overrides: {} })
  }
  return createOpenAiMessageOverridesStrategy({
    apiKey: messageOverridesStrategyApiKey,
    baseUrl: optionalEnv('SLACKBOTV2_MESSAGE_OVERRIDES_OPENAI_BASE_URL'),
    logger: consoleLogger,
    maxOutputTokens: optionalNumberEnv('SLACKBOTV2_MESSAGE_OVERRIDES_MAX_OUTPUT_TOKENS'),
    model: stringEnv('SLACKBOTV2_MESSAGE_OVERRIDES_MODEL', 'gpt-5.4-nano'),
    timeoutMs: optionalNumberEnv('SLACKBOTV2_MESSAGE_OVERRIDES_TIMEOUT_MS')
  })
}

function optionalNumberEnv(name: string): number | undefined {
  const value = optionalEnv(name)
  if (!value) return undefined
  const parsed = Number.parseInt(value, 10)
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`)
  }
  return parsed
}

function log(level: (typeof LOG_LEVELS)[number], message: string, data?: unknown): void {
  if (LOG_LEVELS.indexOf(level) < LOG_LEVELS.indexOf(minLogLevel)) return
  console.log(
    JSON.stringify({
      level,
      service: 'slackbotv2',
      timestamp: new Date().toISOString(),
      event: message,
      ...(data && typeof data === 'object' ? (data as Record<string, unknown>) : {})
    })
  )
}
