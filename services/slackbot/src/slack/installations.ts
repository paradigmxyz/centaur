import { WebClient } from '@slack/web-api'

export type SlackClientOptions = {
  slackApiUrl?: string
}

export type SlackInstallation = {
  teamId?: string
  enterpriseId?: string
  botToken: string
  botUserId?: string
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
  private botUserId?: string

  constructor(opts: { token?: string; slackApiUrl?: string }) {
    this.token = opts.token
    this.slackApiUrl = opts.slackApiUrl
  }

  async findInstallation(key: SlackInstallationKey): Promise<SlackInstallation | null> {
    if (!this.token) return null
    this.botUserId ??= await fetchBotUserId(this.token, {
      slackApiUrl: this.slackApiUrl
    })
    return {
      teamId: key.teamId,
      enterpriseId: key.enterpriseId,
      botToken: this.token,
      botUserId: this.botUserId
    }
  }
}

async function fetchBotUserId(
  token: string,
  opts: SlackClientOptions = {}
): Promise<string | undefined> {
  const auth = await createSlackWebClient(token, opts).auth.test()
  return typeof auth.user_id === 'string' ? auth.user_id : undefined
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

export function createSlackWebClient(
  token: string,
  opts: SlackClientOptions = {}
): WebClient {
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
