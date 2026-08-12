import { buildFeishuClientConfig } from './feishu-events.js'

export type FeishuBotConfig = {
  feishu: ReturnType<typeof buildFeishuClientConfig>
  botOpenId: string
  tenantAllowlist: ReadonlySet<string>
  apiUrl: string
  apiKey: string
  consolePublicUrl?: string
  port: number
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): FeishuBotConfig {
  const appId = required(env, 'FEISHU_APP_ID')
  const appSecret = required(env, 'FEISHU_APP_SECRET')
  const tenantAllowlist = new Set(
    required(env, 'FEISHU_TENANT_ALLOWLIST')
      .split(',')
      .map(value => value.trim())
      .filter(Boolean)
  )
  if (tenantAllowlist.size === 0) throw new Error('FEISHU_TENANT_ALLOWLIST is empty')
  const apiUrl = new URL(required(env, 'CENTAUR_API_URL')).toString()
  const consolePublicUrl = optionalUrl(env.CENTAUR_CONSOLE_PUBLIC_URL)
  const rawPort = env.PORT?.trim() || '3005'
  const port = Number.parseInt(rawPort, 10)
  if (!Number.isInteger(port) || port < 1 || port > 65_535) throw new Error('PORT is invalid')
  return {
    feishu: buildFeishuClientConfig(appId, appSecret),
    botOpenId: required(env, 'FEISHU_BOT_OPEN_ID'),
    tenantAllowlist,
    apiUrl,
    apiKey: required(env, 'FEISHUBOT_API_KEY'),
    ...(consolePublicUrl ? { consolePublicUrl } : {}),
    port
  }
}

function required(env: NodeJS.ProcessEnv, name: string): string {
  const value = env[name]?.trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function optionalUrl(value: string | undefined): string | undefined {
  if (!value?.trim()) return undefined
  return new URL(value.trim()).toString().replace(/\/$/u, '')
}
