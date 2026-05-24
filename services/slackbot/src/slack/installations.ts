import { WebClient } from '@slack/web-api'

export type SlackClientOptions = {
  slackApiUrl?: string
}

export type SlackInstallation = {
  teamId?: string
  enterpriseId?: string
  botToken: string
  botUserId?: string
  botId?: string
}

export type SlackInstallationKey = {
  teamId?: string
  enterpriseId?: string
}

export interface SlackInstallationStore {
  findInstallation(key: SlackInstallationKey): Promise<SlackInstallation | null>
}

export class EnvSlackInstallationStore implements SlackInstallationStore {
  readonly token?: string
  private readonly slackApiUrl?: string
  private botIdentity?: SlackBotIdentity

  constructor(opts: { token?: string; slackApiUrl?: string }) {
    this.token = opts.token
    this.slackApiUrl = opts.slackApiUrl
  }

  async findInstallation(key: SlackInstallationKey): Promise<SlackInstallation | null> {
    if (!this.token) return null
    this.botIdentity ??= await fetchBotIdentity(this.token, {
      slackApiUrl: this.slackApiUrl
    })
    return {
      teamId: key.teamId,
      enterpriseId: key.enterpriseId,
      botToken: this.token,
      botUserId: this.botIdentity.botUserId,
      botId: this.botIdentity.botId
    }
  }
}

type SlackBotIdentity = {
  botUserId?: string
  botId?: string
}

async function fetchBotIdentity(
  token: string,
  opts: SlackClientOptions = {}
): Promise<SlackBotIdentity> {
  const auth = await createSlackWebClient(token, opts).auth.test()
  const botId = (auth as { bot_id?: unknown }).bot_id
  return {
    botUserId: typeof auth.user_id === 'string' ? auth.user_id : undefined,
    botId: typeof botId === 'string' ? botId : undefined
  }
}

export class SlackClientResolver {
  readonly store: SlackInstallationStore
  private readonly slackApiUrl?: string

  constructor(store: SlackInstallationStore, opts: SlackClientOptions = {}) {
    this.store = store
    this.slackApiUrl = opts.slackApiUrl
  }

  async resolve(
    key: SlackInstallationKey
  ): Promise<{ installation: SlackInstallation; client: WebClient }> {
    const installation = await this.store.findInstallation(key)
    if (!installation) {
      throw new Error(
        `No Slack installation for team=${key.teamId ?? '-'} enterprise=${key.enterpriseId ?? '-'}`
      )
    }
    return {
      installation,
      client: createSlackWebClient(installation.botToken, {
        slackApiUrl: this.slackApiUrl
      })
    }
  }
}

export function createSlackWebClient(token: string, opts: SlackClientOptions = {}): WebClient {
  return new WebClient(
    token,
    opts.slackApiUrl
      ? {
          slackApiUrl: opts.slackApiUrl,
          allowAbsoluteUrls: false
        }
      : undefined
  )
}
