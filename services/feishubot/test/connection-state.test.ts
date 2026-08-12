import { describe, expect, it } from 'bun:test'
import { FeishuConnectionState } from '../src/connection-state.js'

describe('Feishu long-connection readiness', () => {
  it('becomes ready only after the SDK handshake and tracks reconnects', () => {
    let recoveryStarts = 0
    const state = new FeishuConnectionState(() => {
      recoveryStarts += 1
    })
    const callbacks = state.callbacks()

    expect(state.ready).toBe(false)
    callbacks.onReady()
    expect(state.ready).toBe(true)
    expect(recoveryStarts).toBe(1)
    callbacks.onReconnecting()
    expect(state.ready).toBe(false)
    callbacks.onReconnected()
    expect(state.ready).toBe(true)
    expect(recoveryStarts).toBe(1)
    callbacks.onError(new Error('terminal connection failure'))
    expect(state.ready).toBe(false)
  })
})
