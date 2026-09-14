import { describe, expect, it } from 'bun:test';
import { conflateGoogleChatRenderStream, type GoogleChatRenderChunk } from '../src/conflate.js';

type ManualSource = {
  iterable: AsyncIterable<GoogleChatRenderChunk>;
  push(chunk: GoogleChatRenderChunk): void;
  end(): void;
  readonly returnCalled: boolean;
};

describe('conflateGoogleChatRenderStream', () => {
  it('concatenates text while the consumer is busy', async () => {
    const source = manualSource();
    const stream = conflateGoogleChatRenderStream(source.iterable)[Symbol.asyncIterator]();

    source.push({ type: 'text_delta', text: 'Hello ' });
    expect((await stream.next()).value).toEqual({ type: 'text_delta', text: 'Hello ' });

    source.push({ type: 'text_delta', text: 'from ' });
    source.push({ type: 'text_delta', text: 'Chat' });
    await settle();

    expect((await stream.next()).value).toEqual({ type: 'text_delta', text: 'from Chat' });
    source.end();
    expect((await stream.next()).done).toBe(true);
  });

  it('yields pending text before terminal chunks', async () => {
    const source = manualSource();
    const stream = conflateGoogleChatRenderStream(source.iterable)[Symbol.asyncIterator]();

    source.push({ type: 'text_delta', text: 'final text' });
    source.push({ type: 'done' });
    await settle();

    expect((await stream.next()).value).toEqual({ type: 'text_delta', text: 'final text' });
    expect((await stream.next()).value).toEqual({ type: 'done' });
    source.end();
    expect((await stream.next()).done).toBe(true);
  });

  it('cancels the source when abandoned', async () => {
    const source = manualSource();
    const stream = conflateGoogleChatRenderStream(source.iterable)[Symbol.asyncIterator]();

    source.push({ type: 'text_delta', text: 'start' });
    await stream.next();
    await stream.return?.(undefined);
    await settle();

    expect(source.returnCalled).toBe(true);
  });
});

function manualSource(): ManualSource {
  const queue: GoogleChatRenderChunk[] = [];
  let closed = false;
  let notify: (() => void) | undefined;
  let returnCalled = false;

  const iterator: AsyncIterator<GoogleChatRenderChunk> = {
    async next() {
      while (true) {
        const chunk = queue.shift();
        if (chunk) {
          return { done: false, value: chunk };
        }
        if (closed) {
          return { done: true, value: undefined };
        }
        await new Promise<void>((resolve) => {
          notify = resolve;
        });
        notify = undefined;
      }
    },
    async return() {
      returnCalled = true;
      closed = true;
      notify?.();
      return { done: true, value: undefined };
    },
  };

  return {
    iterable: { [Symbol.asyncIterator]: () => iterator },
    push(chunk) {
      queue.push(chunk);
      notify?.();
    },
    end() {
      closed = true;
      notify?.();
    },
    get returnCalled() {
      return returnCalled;
    },
  };
}

async function settle(): Promise<void> {
  for (let index = 0; index < 20; index += 1) {
    await Promise.resolve();
  }
}
