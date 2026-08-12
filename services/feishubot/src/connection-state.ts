export type FeishuConnectionCallbacks = {
  onReady: () => void
  onReconnecting: () => void
  onReconnected: () => void
  onError: (error: Error) => void
}

export class FeishuConnectionState {
  private isReady = false
  private recoveryStarted = false

  constructor(private readonly startRecovery: () => void) {}

  get ready(): boolean {
    return this.isReady
  }

  callbacks(): FeishuConnectionCallbacks {
    return {
      onReady: () => {
        this.isReady = true
        if (!this.recoveryStarted) {
          this.recoveryStarted = true
          this.startRecovery()
        }
      },
      onReconnecting: () => {
        this.isReady = false
      },
      onReconnected: () => {
        this.isReady = true
      },
      onError: () => {
        this.isReady = false
      }
    }
  }

  stop(): void {
    this.isReady = false
  }
}
