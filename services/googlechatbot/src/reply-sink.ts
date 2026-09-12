import type { GoogleChatAdapter } from '@chat-adapter/gchat';
import type { Thread } from 'chat';

const THINKING_TEXT = 'Thinking...';

/**
 * Google Chat rejects messages longer than 4096 characters, so a long answer is
 * delivered as the edited progress message plus follow-up messages in the same
 * thread.
 */
export const GOOGLE_CHAT_MAX_MESSAGE_CHARS = 4096;

export type GoogleChatReplySink = {
  begin(): Promise<{ progressMessageId?: string }>;
  emit(delta: string, fullText: string): Promise<GoogleChatReplySinkResult>;
  complete(finalText: string, fullText: string): Promise<GoogleChatReplySinkResult>;
  fail(text: string, fullText: string): Promise<GoogleChatReplySinkResult>;
};

export type GoogleChatReplySinkResult = void | { progressMessageId?: string };

export type GoogleChatReplyPort = {
  initialMessageId?: string;
  minEditIntervalMs?: number;
  now?: () => number;
  post(text: string): Promise<{ id?: string } | unknown>;
  update(messageId: string, text: string): Promise<unknown>;
};

export function createChatReplySink(
  thread: Thread,
  options: { minEditIntervalMs?: number } = {},
): GoogleChatReplySink {
  return createStreamingEditReplySink({
    minEditIntervalMs: options.minEditIntervalMs,
    post: async (text) => thread.adapter.postMessage(thread.id, { markdown: text }),
    update: async (messageId, text) => thread.adapter.editMessage(thread.id, messageId, { markdown: text }),
  });
}

export function createAdapterReplySink(
  adapter: GoogleChatAdapter,
  threadId: string,
  messageId: string | undefined,
  options: { minEditIntervalMs?: number } = {},
): GoogleChatReplySink {
  return createStreamingEditReplySink({
    initialMessageId: messageId,
    minEditIntervalMs: options.minEditIntervalMs,
    post: async (text) => adapter.postMessage(threadId, { markdown: text }),
    update: async (messageId, text) => adapter.editMessage(threadId, messageId, { markdown: text }),
  });
}

/**
 * Google Chat has no streaming surface, so the answer is rendered by editing one
 * progress message. Edits are throttled: `spaces.messages.update` is rate
 * limited per space, and an unthrottled token stream would exhaust the quota
 * long before the answer is complete.
 */
export function createStreamingEditReplySink(port: GoogleChatReplyPort): GoogleChatReplySink {
  const now = port.now ?? (() => Date.now());
  const minEditIntervalMs = port.minEditIntervalMs ?? 0;
  let progressMessageId = port.initialMessageId;
  let flushedText = progressMessageId ? THINKING_TEXT : '';
  let lastEditAt = progressMessageId ? now() : 0;

  async function write(text: string): Promise<void> {
    const [head, ...rest] = splitGoogleChatMessage(text);
    progressMessageId = await updateOrPost(port, progressMessageId, head ?? '');
    flushedText = text;
    lastEditAt = now();
    for (const chunk of rest) {
      await port.post(chunk);
    }
  }

  return {
    async begin() {
      if (!progressMessageId) {
        const posted = await port.post(THINKING_TEXT);
        progressMessageId = messageIdOf(posted);
        flushedText = progressMessageId ? THINKING_TEXT : '';
        lastEditAt = now();
      }
      return { progressMessageId };
    },
    async emit(_delta, fullText) {
      if (!fullText || fullText === flushedText) {
        return { progressMessageId };
      }
      if (now() - lastEditAt < minEditIntervalMs) {
        return { progressMessageId };
      }
      // Intermediate renders only ever show the first chunk: follow-up messages
      // cannot be retracted, so partial overflow is held back until completion.
      const [head] = splitGoogleChatMessage(fullText);
      if (head === flushedText) {
        return { progressMessageId };
      }
      progressMessageId = await updateOrPost(port, progressMessageId, head ?? '');
      // Track what was actually rendered, not the text it came from: an
      // over-long answer still owes its overflow messages at completion.
      flushedText = head ?? '';
      lastEditAt = now();
      return { progressMessageId };
    },
    async complete(finalText, fullText) {
      const text = finalText || fullText;
      if (text && text !== flushedText) {
        await write(text);
      }
      return { progressMessageId };
    },
    async fail(text) {
      await write(text);
      return { progressMessageId };
    },
  };
}

/**
 * Split rendered markdown into Google Chat sized messages, preferring paragraph
 * then line boundaries so code fences and lists survive the cut where possible.
 */
export function splitGoogleChatMessage(
  text: string,
  maxChars = GOOGLE_CHAT_MAX_MESSAGE_CHARS,
): string[] {
  if (text.length <= maxChars) {
    return [text];
  }
  const chunks: string[] = [];
  let rest = text;
  while (rest.length > maxChars) {
    const window = rest.slice(0, maxChars);
    const splitAt = lastBoundary(window, maxChars);
    chunks.push(rest.slice(0, splitAt).trimEnd());
    rest = rest.slice(splitAt).trimStart();
  }
  if (rest) {
    chunks.push(rest);
  }
  return chunks;
}

function lastBoundary(window: string, maxChars: number): number {
  const paragraph = window.lastIndexOf('\n\n');
  if (paragraph > maxChars / 2) {
    return paragraph;
  }
  const line = window.lastIndexOf('\n');
  if (line > maxChars / 2) {
    return line;
  }
  const space = window.lastIndexOf(' ');
  return space > maxChars / 2 ? space : maxChars;
}

async function updateOrPost(
  port: Pick<GoogleChatReplyPort, 'post' | 'update'>,
  messageId: string | undefined,
  text: string,
): Promise<string | undefined> {
  if (!messageId) {
    return messageIdOf(await port.post(text));
  }
  try {
    await port.update(messageId, text);
    return messageId;
  } catch {
    return messageIdOf(await port.post(text));
  }
}

function messageIdOf(value: unknown): string | undefined {
  return typeof value === 'object' && value !== null && 'id' in value
    ? String((value as { id?: unknown }).id ?? '') || undefined
    : undefined;
}
