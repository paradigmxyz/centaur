import { Buffer } from 'node:buffer'
import { timingSafeEqual } from 'node:crypto'
import type { SlackSocketModeEvent, SlackSocketModeHandlers } from '@chat-adapter/slack'
import type { Logger } from 'chat'

/**
 * Header carrying the per-process token that authenticates a Socket Mode event
 * replayed into this service's own Slack webhook routes.
 *
 * Socket Mode payloads arrive unsigned over an authenticated WebSocket, so the
 * adapter's signature verification is replaced by this token: only this process
 * knows it, and a request reaching the routes without it is rejected with 401.
 * Replaying through the routes, rather than letting the adapter dispatch socket
 * events straight to the handlers, keeps one code path for the external-org
 * allowlist, channel auto-join, late-file repair and webhook metrics.
 */
export const SLACK_SOCKET_LOOPBACK_HEADER = 'x-centaur-slack-socket-token'

/** Origin of replayed requests; only the path is ever routed. */
const LOOPBACK_ORIGIN = 'http://slackbotv2.socket.internal'

const CONNECT_INITIAL_DELAY_MS = 500
const CONNECT_MAX_DELAY_MS = 30_000

export type SlackSocketStatus = {
  attempts: number
  connected: boolean
}

/** How the runner opens and closes the Slack WebSocket. */
export type SlackSocketTransport = {
  /** The adapter's `connectSocketMode`; rejects when Slack refuses the token. */
  connect: (handlers: SlackSocketModeHandlers) => Promise<unknown>
  /** The adapter's `disconnect`; closes the socket it opened. */
  disconnect: () => Promise<unknown>
}

export type SlackSocketRunner = {
  /**
   * Waits for `ready`, then connects. Settles after the first attempt and never
   * rejects; a failed attempt keeps retrying with capped exponential backoff.
   */
  start(): Promise<void>
  status(): SlackSocketStatus
  stop(): Promise<void>
}

export function createSlackSocketRunner(
  input: SlackSocketTransport & {
    /** Replays a Slack payload through the service's own webhook routes. */
    dispatch: (request: Request) => Promise<Response>
    logger: Logger
    loopbackToken: string
    /** Gate on the state backend so events never land on a disconnected database. */
    ready?: Promise<unknown>
  }
): SlackSocketRunner {
  const { logger } = input
  const status: SlackSocketStatus = { attempts: 0, connected: false }
  let startPromise: Promise<void> | undefined
  let stopped = false

  const markConnected = (): void => {
    if (!status.connected) {
      logger.info('slackbotv2_slack_socket_connected', { attempts: status.attempts })
    }
    status.connected = true
  }

  const markDisconnected = (event: string): void => {
    if (status.connected) logger.warn('slackbotv2_slack_socket_disconnected', { event })
    status.connected = false
  }

  const handlers: SlackSocketModeHandlers = {
    onEvent: async (event: SlackSocketModeEvent) => {
      // Slack drops the connection when an event is not acked within 3s, and a
      // handoff can take longer than that, so ack before dispatching. No
      // handler here answers through the ack payload.
      await event.ack()
      // A retry means Slack never saw the ack for a delivery already dispatched;
      // replaying it would hand the same message off twice.
      if ((event.retryNum ?? 0) > 0) return
      const request = slackSocketRequest(event.eventType, event.body, input.loopbackToken)
      if (!request) return
      try {
        const response = await input.dispatch(request)
        if (!response.ok) {
          logger.warn('slackbotv2_slack_socket_dispatch_rejected', {
            event_type: event.eventType,
            status: response.status
          })
        }
      } catch (error) {
        logger.error('slackbotv2_slack_socket_dispatch_failed', {
          error: errorText(error),
          event_type: event.eventType
        })
      }
    },
    onStateChange: state => {
      if (state === 'connected') markConnected()
      else if (state === 'disconnected' || state === 'reconnecting') markDisconnected(state)
    }
  }

  const connect = async (): Promise<boolean> => {
    status.attempts += 1
    try {
      await input.connect(handlers)
      markConnected()
      return true
    } catch (error) {
      markDisconnected('connect_failed')
      logger.error('slackbotv2_slack_socket_connect_failed', {
        attempts: status.attempts,
        error: errorText(error)
      })
      await input.disconnect().catch(() => undefined)
      return false
    }
  }

  const retryConnect = async (): Promise<void> => {
    for (let attempt = 0; !stopped; attempt++) {
      await sleep(Math.min(CONNECT_INITIAL_DELAY_MS * 2 ** attempt, CONNECT_MAX_DELAY_MS))
      if (stopped || (await connect())) return
    }
  }

  return {
    start(): Promise<void> {
      startPromise ??= (async () => {
        if (input.ready) await input.ready.catch(() => undefined)
        if (stopped) return
        if (!(await connect())) void retryConnect()
      })()
      return startPromise
    },
    status(): SlackSocketStatus {
      return { ...status }
    },
    async stop(): Promise<void> {
      stopped = true
      await input.disconnect().catch(() => undefined)
      markDisconnected('stopped')
    }
  }
}

/**
 * Translates a Socket Mode envelope into the HTTP request Slack would have
 * POSTed for the same event, so replayed events hit the same route, gates and
 * metrics as webhook deliveries. Null for envelope types with no route.
 */
export function slackSocketRequest(
  eventType: string,
  body: unknown,
  loopbackToken: string
): Request | null {
  if (typeof body !== 'object' || body === null) return null
  switch (eventType) {
    case 'events_api':
      return loopbackRequest(
        '/api/slack/events',
        'application/json',
        JSON.stringify(body),
        loopbackToken
      )
    case 'interactive':
      return loopbackRequest(
        '/api/slack/actions',
        'application/x-www-form-urlencoded',
        new URLSearchParams({ payload: JSON.stringify(body) }).toString(),
        loopbackToken
      )
    case 'slash_commands':
      return loopbackRequest(
        '/api/slack/commands',
        'application/x-www-form-urlencoded',
        formParams(body as Record<string, unknown>).toString(),
        loopbackToken
      )
    default:
      return null
  }
}

/**
 * Webhook verifier used in place of signature checking while Socket Mode is on:
 * accepts only requests this process replayed, rejecting anything that reaches
 * the routes from outside.
 */
export function slackSocketLoopbackVerifier(loopbackToken: string): (request: Request) => boolean {
  const expected = Buffer.from(loopbackToken, 'utf8')
  return request => {
    const provided = Buffer.from(request.headers.get(SLACK_SOCKET_LOOPBACK_HEADER) ?? '', 'utf8')
    return provided.length === expected.length && timingSafeEqual(provided, expected)
  }
}

function loopbackRequest(
  path: string,
  contentType: string,
  body: string,
  loopbackToken: string
): Request {
  return new Request(`${LOOPBACK_ORIGIN}${path}`, {
    body,
    headers: { 'content-type': contentType, [SLACK_SOCKET_LOOPBACK_HEADER]: loopbackToken },
    method: 'POST'
  })
}

function formParams(body: Record<string, unknown>): URLSearchParams {
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(body)) {
    if (typeof value === 'string') params.set(key, value)
  }
  return params
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}
