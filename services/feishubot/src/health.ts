import { FeishuMetrics } from './metrics.js'

const defaultMetrics = new FeishuMetrics()

export function healthResponse(
  path: string,
  ready: boolean,
  metrics: FeishuMetrics = defaultMetrics
): Response | undefined {
  if (path === '/health') return Response.json({ ok: true })
  if (path === '/ready') {
    return Response.json({ ready }, { status: ready ? 200 : 503 })
  }
  if (path === '/metrics') {
    return new Response(metrics.prometheus(ready), {
      headers: { 'content-type': 'text/plain; version=0.0.4; charset=utf-8' }
    })
  }
  return undefined
}
