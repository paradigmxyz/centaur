import { AsyncLocalStorage } from 'node:async_hooks'

export type SlackbotV2RequestContext = {
  slackTeamId?: string
  waitUntil(promise: Promise<unknown>): void
}

export const requestContext = new AsyncLocalStorage<SlackbotV2RequestContext>()
