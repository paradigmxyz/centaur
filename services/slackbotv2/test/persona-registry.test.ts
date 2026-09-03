import { describe, expect, test } from 'bun:test'
import { createPersonaIdsResolver } from '../src/persona-registry'

describe('createPersonaIdsResolver', () => {
  test('loads, validates, sorts, and caches deployed persona ids', async () => {
    let requestCount = 0
    const resolve = createPersonaIdsResolver({
      apiKey: 'test-key',
      apiUrl: 'http://api.example.test',
      fetch: (async (input: RequestInfo | URL, init?: RequestInit) => {
        requestCount += 1
        expect(String(input)).toBe('http://api.example.test/api/personas')
        expect(init?.headers).toEqual({ authorization: 'Bearer test-key' })
        return Response.json(['invest', 'eng', 'invest', 'not a persona'])
      }) as unknown as typeof fetch
    })

    await expect(resolve()).resolves.toEqual(['eng', 'invest'])
    await expect(resolve()).resolves.toEqual(['eng', 'invest'])
    expect(requestCount).toBe(1)
  })

  test('keeps a stale catalog when refresh fails', async () => {
    let requestCount = 0
    const resolve = createPersonaIdsResolver({
      apiUrl: 'http://api.example.test',
      failureBackoffMs: 60_000,
      fetch: (async () => {
        requestCount += 1
        if (requestCount === 1) return Response.json(['invest'])
        return new Response('unavailable', { status: 503 })
      }) as unknown as typeof fetch,
      ttlMs: 0
    })

    await expect(resolve()).resolves.toEqual(['invest'])
    await expect(resolve()).resolves.toEqual(['invest'])
    expect(requestCount).toBe(2)
  })
})
