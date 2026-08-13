import * as Lark from '@larksuiteoapi/node-sdk'
import { FeishuBot } from './bot.js'
import { FeishuConnectionState } from './connection-state.js'
import { loadConfig } from './config.js'
import { FeishuRenderer } from './feishu-client.js'
import { FeishuRenderRecovery } from './render-recovery.js'
import { FeishuSessionApi } from './session-api.js'
import { healthResponse } from './health.js'
import { FeishuMetrics } from './metrics.js'

const config = loadConfig()
const client = new Lark.Client(config.feishu)
const metrics = new FeishuMetrics()
const renderer = new FeishuRenderer(client, metrics)
const api = new FeishuSessionApi({
  baseUrl: config.apiUrl,
  apiKey: config.apiKey
})
const bot = new FeishuBot({
  botOpenId: config.botOpenId,
  tenantAllowlist: config.tenantAllowlist,
  api,
  renderer,
  metrics,
  ...(config.consolePublicUrl ? { consolePublicUrl: config.consolePublicUrl } : {})
})
const dispatcher = new Lark.EventDispatcher({})
dispatcher.register({
  'im.message.receive_v1': (event: unknown) => {
    const startedAt = performance.now()
    bot.acceptMessageEvent(event)
    metrics.recordEventAck('message', performance.now() - startedAt)
  },
  'card.action.trigger': (event: unknown) => {
    const startedAt = performance.now()
    bot.acceptCardEvent(event)
    metrics.recordEventAck('card', performance.now() - startedAt)
  }
})

const recovery = new FeishuRenderRecovery(bot, 5_000, metrics)
const connection = new FeishuConnectionState(() => {
  void recovery.run()
})
const ws = new Lark.WSClient({
  ...config.feishu,
  ...connection.callbacks()
})
const server = Bun.serve({
  port: config.port,
  fetch(request) {
    const path = new URL(request.url).pathname
    return healthResponse(path, connection.ready, metrics) ?? new Response('Not found', { status: 404 })
  }
})

void ws.start({ eventDispatcher: dispatcher }).catch(() => {
  console.error('China Feishu long connection failed')
})

for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.once(signal, () => {
    connection.stop()
    recovery.stop()
    ws.close()
    void server.stop(true).finally(() => process.exit(0))
  })
}
