import { centaurApiKey, type AppConfig } from '../config'
import { clientSpanOptions, injectTraceHeaders, spanAttributes, withSpan } from '../otel'

export type CentaurRuntimeLookupResult =
  | { ok: true; active: boolean; status: number }
  | { ok: false; active: false; status?: number; error: string }

export async function lookupActiveRuntimeAssignment(
  config: AppConfig,
  threadKey: string
): Promise<CentaurRuntimeLookupResult> {
  return withSpan(
    'centaur.slackbot.runtime_lookup',
    clientSpanOptions({
      'centaur.thread_key': threadKey
    }),
    async span => {
      try {
        const url = new URL('/agent/runtime', config.CENTAUR_API_URL)
        url.searchParams.set('key', threadKey)
        const apiKey = centaurApiKey(config)
        const response = await fetch(url, {
          method: 'GET',
          headers: {
            'X-Centaur-Thread-Key': threadKey,
            ...injectTraceHeaders(),
            ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {})
          }
        })
        spanAttributes(span, {
          'http.response.status_code': response.status
        })
        if (!response.ok) {
          return {
            ok: false,
            active: false,
            status: response.status,
            error: await response.text()
          }
        }
        const body = (await response.json()) as {
          assignment_generation?: unknown
          runtime_id?: unknown
        }
        const active =
          (body.assignment_generation !== null && body.assignment_generation !== undefined) ||
          (typeof body.runtime_id === 'string' && body.runtime_id.trim().length > 0)
        spanAttributes(span, {
          'centaur.runtime.active_assignment': active
        })
        return { ok: true, active, status: response.status }
      } catch (error) {
        return {
          ok: false,
          active: false,
          error: error instanceof Error ? error.message : String(error)
        }
      }
    }
  )
}
