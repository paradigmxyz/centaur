export interface FeishuDeliveryReconciler {
  listPendingDeliveries(): Promise<string[]>
  reconcileDelivery(threadKey: string): Promise<void>
}

export class FeishuRenderRecovery {
  private stopped = false

  constructor(
    private readonly reconciler: FeishuDeliveryReconciler,
    private readonly intervalMs = 5_000
  ) {}

  stop(): void {
    this.stopped = true
  }

  async run(): Promise<void> {
    while (!this.stopped) {
      try {
        for (const threadKey of await this.reconciler.listPendingDeliveries()) {
          await this.reconciler.reconcileDelivery(threadKey)
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
