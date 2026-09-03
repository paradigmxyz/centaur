import type { Logger } from 'chat'

const DEFAULT_TTL_MS = 5 * 60 * 1000
const DEFAULT_FAILURE_BACKOFF_MS = 30 * 1000
const DEFAULT_TIMEOUT_MS = 2 * 1000
const PERSONA_ID_PATTERN = /^[A-Za-z0-9._-]+$/

export type PersonaIdsResolverOptions = {
  apiKey?: string
  apiUrl: string
  failureBackoffMs?: number
  fetch?: typeof fetch
  logger?: Logger
  timeoutMs?: number
  ttlMs?: number
}

/**
 * Cache the API-owned persona catalog so bare --<persona-id> selectors remain
 * deterministic without hard-coding deployment-specific persona names in the
 * Slack ingress.
 */
export function createPersonaIdsResolver(
  options: PersonaIdsResolverOptions
): () => Promise<readonly string[]> {
  const fetchFn = options.fetch ?? fetch
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
  const ttlMs = options.ttlMs ?? DEFAULT_TTL_MS
  const failureBackoffMs = options.failureBackoffMs ?? DEFAULT_FAILURE_BACKOFF_MS
  let cached: readonly string[] = []
  let expiresAt = 0
  let retryAt = 0
  let pending: Promise<readonly string[]> | undefined

  return async () => {
    const now = Date.now()
    if (now < expiresAt || now < retryAt) return cached
    if (pending) return pending

    pending = fetchPersonaIds({
      apiKey: options.apiKey,
      apiUrl: options.apiUrl,
      fetchFn,
      timeoutMs
    })
      .then(personaIds => {
        cached = personaIds
        expiresAt = Date.now() + ttlMs
        retryAt = 0
        return cached
      })
      .catch(error => {
        retryAt = Date.now() + failureBackoffMs
        options.logger?.warn('slackbotv2_persona_registry_request_failed', {
          error: error instanceof Error ? error.message : String(error),
          timeout_ms: timeoutMs
        })
        if (cached.length > 0) return cached
        throw error
      })
      .finally(() => {
        pending = undefined
      })
    return pending
  }
}

async function fetchPersonaIds(input: {
  apiKey?: string
  apiUrl: string
  fetchFn: typeof fetch
  timeoutMs: number
}): Promise<readonly string[]> {
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), input.timeoutMs)
  try {
    const response = await input.fetchFn(
      new URL('/api/personas', ensureTrailingSlash(input.apiUrl)),
      {
        headers: input.apiKey ? { authorization: `Bearer ${input.apiKey}` } : {},
        signal: controller.signal
      }
    )
    if (!response.ok) {
      throw new Error(
        `persona registry request failed with HTTP ${response.status} ${response.statusText}`
      )
    }
    const value: unknown = await response.json()
    if (!Array.isArray(value)) throw new Error('persona registry response must be an array')
    return Array.from(
      new Set(
        value.filter(
          (personaId): personaId is string =>
            typeof personaId === 'string' && PERSONA_ID_PATTERN.test(personaId)
        )
      )
    ).sort()
  } finally {
    clearTimeout(timeout)
  }
}

function ensureTrailingSlash(value: string): string {
  return value.endsWith('/') ? value : `${value}/`
}
