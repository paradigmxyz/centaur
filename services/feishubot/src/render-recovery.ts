export interface FeishuDeliveryReconciler {
  listPendingDeliveries(): Promise<string[]>
  reconcileDelivery(threadKey: string): Promise<void>
}

export class FeishuRenderRecovery {
  private stopped = false

  constructor(
    private readonly reconciler: FeishuDeliveryReconciler,
    private readonly intervalMs = 5_000,
    private readonly metrics: FeishuMetrics = new FeishuMetrics()
  ) {}

  stop(): void {
    this.stopped = true
  }

  async run(): Promise<void> {
    while (!this.stopped) {
      try {
        for (const threadKey of await this.reconciler.listPendingDeliveries()) {
          this.metrics.recordRecovery('attempted')
          try {
            await this.reconciler.reconcileDelivery(threadKey)
            this.metrics.recordRecovery('succeeded')
          } catch (error) {
            this.metrics.recordRecovery('failed')
            throw error
          }
        }
      } catch (error) {
        console.error('Feishu delivery reconciliation failed', {
          status: error instanceof Error ? error.name : 'unknown'
        })
      }
      await Bun.sleep(this.intervalMs)
    }
  }
}
import { FeishuMetrics } from './metrics.js'
