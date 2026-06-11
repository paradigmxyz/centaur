import { describe, expect, it } from 'bun:test'
import type { ChatSDKStreamChunk } from '@centaur/rendering'
import { conflateChatSdkStream } from '../src/conflate'

type ManualSource = {
  iterable: AsyncIterable<ChatSDKStreamChunk>
  push(chunk: ChatSDKStreamChunk): void
  end(): void
  fail(error: Error): void
  readonly returnCalled: boolean
}

function manualSource(): ManualSource {
  const queue: ChatSDKStreamChunk[] = []
  let closed = false
  let failure: Error | undefined
  let notify: (() => void) | undefined
  let returnCalled = false

  const iterator: AsyncIterator<ChatSDKStreamChunk> = {
    async next() {
      while (true) {
        const chunk = queue.shift()
        if (chunk) return { done: false, value: chunk }
        if (failure) throw failure
        if (closed) return { done: true, value: undefined }
        await new Promise<void>(resolve => {
          notify = resolve
        })
        notify = undefined
      }
    },
    async return() {
      returnCalled = true
      closed = true
      notify?.()
      return { done: true, value: undefined }
    }
  }

  return {
    iterable: { [Symbol.asyncIterator]: () => iterator },
    push(chunk) {
      queue.push(chunk)
      notify?.()
    },
    end() {
      closed = true
      notify?.()
    },
    fail(error) {
      failure = error
      notify?.()
    },
    get returnCalled() {
      return returnCalled
    }
  }
}

/** Lets the pump drain everything currently queued in the source. */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve()
}

function task(
  id: string,
  status: 'pending' | 'in_progress' | 'complete',
  details?: string
): ChatSDKStreamChunk {
  return { type: 'task_update', id, title: `Task ${id}`, status, ...(details ? { details } : {}) }
}

function markdown(text: string): ChatSDKStreamChunk {
  return { type: 'markdown_text', text }
}

describe('conflateChatSdkStream', () => {
  it('collapses task updates and concatenates markdown while the consumer is busy', async () => {
    const source = manualSource()
    const stream = conflateChatSdkStream(source.iterable)[Symbol.asyncIterator]()

    source.push(task('a', 'pending'))
    const first = await stream.next()
    expect(first.value).toEqual(task('a', 'pending'))

    // Consumer is "busy": everything below folds into the pending snapshot.
    source.push(task('a', 'in_progress', 'running step 1'))
    source.push(markdown('Hello '))
    source.push(task('a', 'complete'))
    source.push(markdown('world'))
    source.push(task('b', 'complete'))
    source.push({ type: 'plan_update', title: 'The plan' })
    await settle()

    expect((await stream.next()).value).toEqual({ type: 'plan_update', title: 'The plan' })
    // Latest status wins; details from the intermediate update survive the
    // merge because the completion chunk omitted them ("unchanged").
    expect((await stream.next()).value).toEqual(task('a', 'complete', 'running step 1'))
    expect((await stream.next()).value).toEqual(task('b', 'complete'))
    expect((await stream.next()).value).toEqual(markdown('Hello world'))

    source.end()
    expect((await stream.next()).done).toBe(true)
  })

  it('merges task fields so omitted details inherit the pending value', async () => {
    const source = manualSource()
    const stream = conflateChatSdkStream(source.iterable)[Symbol.asyncIterator]()

    source.push(markdown('start'))
    expect((await stream.next()).value).toEqual(markdown('start'))

    source.push(task('a', 'in_progress', 'first details'))
    source.push(task('a', 'in_progress', 'second details'))
    source.push(task('a', 'complete'))
    await settle()

    expect((await stream.next()).value).toEqual(task('a', 'complete', 'second details'))
    source.end()
    expect((await stream.next()).done).toBe(true)
  })

  it('passes chunks through unchanged when the consumer keeps up', async () => {
    const source = manualSource()
    const stream = conflateChatSdkStream(source.iterable)[Symbol.asyncIterator]()
    const chunks: ChatSDKStreamChunk[] = [
      task('a', 'pending'),
      markdown('first'),
      task('a', 'complete'),
      markdown('second')
    ]

    for (const chunk of chunks) {
      source.push(chunk)
      expect((await stream.next()).value).toEqual(chunk)
    }
    source.end()
    expect((await stream.next()).done).toBe(true)
  })

  it('keeps tasks in first-seen order when an earlier task updates later', async () => {
    const source = manualSource()
    const stream = conflateChatSdkStream(source.iterable)[Symbol.asyncIterator]()

    // Prime the pump so the chunks below all fold while the consumer is busy.
    source.push(markdown('start'))
    expect((await stream.next()).value).toEqual(markdown('start'))

    source.push(task('a', 'in_progress'))
    source.push(task('b', 'pending'))
    source.push(task('a', 'complete'))
    await settle()

    expect((await stream.next()).value).toEqual(task('a', 'complete'))
    expect((await stream.next()).value).toEqual(task('b', 'pending'))
    source.end()
    expect((await stream.next()).done).toBe(true)
  })

  it('drains pending chunks before surfacing a source failure', async () => {
    const source = manualSource()
    const stream = conflateChatSdkStream(source.iterable)[Symbol.asyncIterator]()

    source.push(markdown('partial answer'))
    source.fail(new Error('sse exploded'))
    await settle()

    expect((await stream.next()).value).toEqual(markdown('partial answer'))
    expect(stream.next()).rejects.toThrow('sse exploded')
  })

  it('cancels the source when the consumer abandons the stream', async () => {
    const source = manualSource()
    const stream = conflateChatSdkStream(source.iterable)[Symbol.asyncIterator]()

    source.push(task('a', 'pending'))
    await stream.next()
    await stream.return?.(undefined)
    await settle()

    expect(source.returnCalled).toBe(true)
  })
})
