import { afterEach, describe, expect, it, spyOn } from 'bun:test'
import { FeishuMetrics } from '../src/metrics.js'
import { FeishuRenderRecovery } from '../src/render-recovery.js'

afterEach(() => {
  spyOn(console, 'error').mockRestore()
})

describe('Feishu render recovery', () => {
  it('continues reconciling after one delivery fails', async () => {
    spyOn(console, 'error').mockImplementation(() => {})
    const attempts: string[] = []
    const metrics = new FeishuMetrics()
    let recovery: FeishuRenderRecovery
    recovery = new FeishuRenderRecovery({
      listPendingDeliveries: async () => ['development:bad', 'development:good'],
      reconcileDelivery: async threadKey => {
        attempts.push(threadKey)
        if (threadKey === 'development:bad') throw new Error('failed delivery')
        recovery.stop()
      }
    }, 1, metrics)

    const running = recovery.run()
    await Bun.sleep(20)
    recovery.stop()
    await running

    expect(attempts).toContain('development:good')
    const output = metrics.prometheus(true)
    expect(output).toContain('centaur_feishubot_recovery_total{outcome="failed"} 1')
    expect(output).toContain('centaur_feishubot_recovery_total{outcome="succeeded"} 1')
  })

  it('starts all pending deliveries without waiting for the first to finish', async () => {
    const attempts: string[] = []
    let releaseSlow: () => void = () => {}
    const slowFinished = new Promise<void>(resolve => {
      releaseSlow = resolve
    })
    let recovery: FeishuRenderRecovery
    recovery = new FeishuRenderRecovery({
      listPendingDeliveries: async () => ['development:slow', 'development:good'],
      reconcileDelivery: async threadKey => {
        attempts.push(threadKey)
        if (threadKey === 'development:slow') await slowFinished
        if (threadKey === 'development:good') recovery.stop()
      }
    }, 1)

    const running = recovery.run()
    await Bun.sleep(10)
    const observedAttempts = [...attempts]
    releaseSlow()
    recovery.stop()
    await running

    expect(observedAttempts).toEqual(['development:slow', 'development:good'])
  })
})
