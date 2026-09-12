import { describe, expect, it } from 'bun:test';
import {
  createStreamingEditReplySink,
  GOOGLE_CHAT_MAX_MESSAGE_CHARS,
  splitGoogleChatMessage,
} from '../src/reply-sink.js';

describe('splitGoogleChatMessage', () => {
  it('keeps messages inside the Google Chat length limit', () => {
    const long = `${'a'.repeat(5_000)}\n\n${'b'.repeat(5_000)}`;
    const chunks = splitGoogleChatMessage(long);

    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(GOOGLE_CHAT_MAX_MESSAGE_CHARS);
    }
    expect(chunks.join('').replaceAll('\n', '')).toBe(long.replaceAll('\n', ''));
  });

  it('prefers paragraph boundaries', () => {
    const text = `${'a'.repeat(60)}\n\n${'b'.repeat(60)}`;
    expect(splitGoogleChatMessage(text, 100)).toEqual(['a'.repeat(60), 'b'.repeat(60)]);
  });

  it('returns a single chunk when the text already fits', () => {
    expect(splitGoogleChatMessage('short')).toEqual(['short']);
  });
});

describe('createStreamingEditReplySink', () => {
  it('posts a placeholder, throttles intermediate edits, and always flushes the final answer', async () => {
    const port = createPort({ minEditIntervalMs: 1_000 });

    const { progressMessageId } = await port.sink.begin();
    await port.sink.emit('PO', 'PO');
    port.advance(100);
    await port.sink.emit('NG', 'PONG');
    port.advance(2_000);
    await port.sink.emit('!', 'PONG!');
    await port.sink.complete('PONG! final', 'PONG!');

    expect(progressMessageId).toBe('message-1');
    expect(port.posts).toEqual(['Thinking...']);
    expect(port.updates).toEqual([
      { id: 'message-1', text: 'PONG!' },
      { id: 'message-1', text: 'PONG! final' },
    ]);
  });

  it('spills an over-long answer into follow-up messages only on completion', async () => {
    const port = createPort({ minEditIntervalMs: 0 });
    const long = `${'a'.repeat(4_000)}\n\n${'b'.repeat(2_000)}`;

    await port.sink.begin();
    await port.sink.emit(long, long);
    expect(port.posts).toEqual(['Thinking...']);

    await port.sink.complete(long, long);

    expect(port.updates.at(-1)?.text.length).toBeLessThanOrEqual(GOOGLE_CHAT_MAX_MESSAGE_CHARS);
    expect(port.posts).toHaveLength(2);
    expect(port.posts.at(-1)).toBe('b'.repeat(2_000));
  });

  it('posts a fresh message when editing the progress message fails', async () => {
    const port = createPort({ minEditIntervalMs: 0, failUpdates: true });

    await port.sink.begin();
    await port.sink.complete('PONG', 'PONG');

    expect(port.posts).toEqual(['Thinking...', 'PONG']);
  });

  it('reuses a recovered progress message id instead of posting again', async () => {
    const port = createPort({ initialMessageId: 'message-42', minEditIntervalMs: 0 });

    const { progressMessageId } = await port.sink.begin();
    await port.sink.complete('Recovered answer', 'Recovered answer');

    expect(progressMessageId).toBe('message-42');
    expect(port.posts).toEqual([]);
    expect(port.updates).toEqual([{ id: 'message-42', text: 'Recovered answer' }]);
  });
});

function createPort(options: {
  failUpdates?: boolean;
  initialMessageId?: string;
  minEditIntervalMs?: number;
}) {
  const posts: string[] = [];
  const updates: Array<{ id: string; text: string }> = [];
  let now = 1_000;
  const sink = createStreamingEditReplySink({
    initialMessageId: options.initialMessageId,
    minEditIntervalMs: options.minEditIntervalMs,
    now: () => now,
    post: async (text) => {
      posts.push(text);
      return { id: `message-${posts.length}` };
    },
    update: async (id, text) => {
      if (options.failUpdates) {
        throw new Error('update failed');
      }
      updates.push({ id, text });
      return undefined;
    },
  });
  return {
    advance: (ms: number) => {
      now += ms;
    },
    posts,
    sink,
    updates,
  };
}
