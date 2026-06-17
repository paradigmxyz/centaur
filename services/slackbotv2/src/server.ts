import { createSlackbotV2, type SlackbotV2Options } from './index'

const port = numberEnv('PORT', 3002)
const apiUrl = stringEnv('CENTAUR_API_URL', 'http://127.0.0.1:8080')
const botToken = requiredEnv('SLACK_BOT_TOKEN')
const signingSecret = requiredEnv('SLACK_SIGNING_SECRET')

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
  botToken,
  botUserId: optionalEnv('SLACK_BOT_USER_ID'),
  defaultHarnessType: optionalEnv('SLACKBOTV2_DEFAULT_HARNESS'),
  idleTimeoutMs: optionalNumberEnv('SESSION_IDLE_TIMEOUT_MS'),
  maxDurationMs: optionalNumberEnv('SESSION_MAX_DURATION_MS'),
  postgresUrl:
    optionalEnv('SLACKBOTV2_DATABASE_URL') ??
    optionalEnv('DATABASE_URL') ??
    optionalEnv('POSTGRES_URL'),
  signingSecret,
  slackApiUrl: optionalEnv('SLACK_API_URL'),
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
