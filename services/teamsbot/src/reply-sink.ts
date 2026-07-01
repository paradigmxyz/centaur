import type { TeamsAdapter } from '@chat-adapter/teams';
import type { Thread } from 'chat';

const THINKING_TEXT = 'Thinking...';

export type TeamsReplySink = {
  begin(): Promise<{ progressActivityId?: string }>;
  emit(delta: string, fullText: string): Promise<TeamsReplySinkResult>;
  complete(finalText: string, fullText: string): Promise<TeamsReplySinkResult>;
  fail(text: string, fullText: string): Promise<TeamsReplySinkResult>;
};

export type TeamsReplySinkResult = void | { progressActivityId?: string };

export function createChatReplySink(thread: Thread, conversationType: string | undefined): TeamsReplySink {
  if (conversationType?.toLowerCase() === 'personal') {
    return createStreamingEditReplySink({
      post: async (text) => thread.adapter.postMessage(thread.id, { markdown: text }),
      update: async (messageId, text) => thread.adapter.editMessage(thread.id, messageId, { markdown: text }),
    });
  }
  return createBlockReplySink({
    post: async (text) => thread.adapter.postMessage(thread.id, { markdown: text }),
    update: async (messageId, text) => thread.adapter.editMessage(thread.id, messageId, { markdown: text }),
  });
}

export function createAdapterBlockReplySink(
  adapter: TeamsAdapter,
  threadId: string,
  activityId: string | undefined,
): TeamsReplySink {
  return createBlockReplySink({
    initialMessageId: activityId,
    post: async (text) => adapter.postMessage(threadId, { markdown: text }),
    update: async (messageId, text) => adapter.editMessage(threadId, messageId, { markdown: text }),
  });
}

function createBlockReplySink(port: {
  initialMessageId?: string;
  post(text: string): Promise<{ id?: string } | unknown>;
  update(messageId: string, text: string): Promise<unknown>;
}): TeamsReplySink {
  let progressActivityId = port.initialMessageId;
  let flushedText = progressActivityId ? THINKING_TEXT : '';
  return {
    async begin() {
      if (!progressActivityId) {
        const posted = await port.post(THINKING_TEXT);
        progressActivityId = activityId(posted);
        flushedText = progressActivityId ? THINKING_TEXT : '';
      }
      return { progressActivityId };
    },
    async emit() {
      return { progressActivityId };
    },
    async complete(finalText) {
      if (finalText !== flushedText) {
        progressActivityId = await updateOrPost(port, progressActivityId, finalText);
        flushedText = finalText;
      }
      return { progressActivityId };
    },
    async fail(text) {
      progressActivityId = await updateOrPost(port, progressActivityId, text);
      flushedText = text;
      return { progressActivityId };
    },
  };
}

function createStreamingEditReplySink(port: {
  post(text: string): Promise<{ id?: string } | unknown>;
  update(messageId: string, text: string): Promise<unknown>;
}): TeamsReplySink {
  let progressActivityId: string | undefined;
  let flushedText = '';
  return {
    async begin() {
      const posted = await port.post(THINKING_TEXT);
      progressActivityId = activityId(posted);
      flushedText = progressActivityId ? THINKING_TEXT : '';
      return { progressActivityId };
    },
    async emit(_delta, fullText) {
      if (fullText && fullText !== flushedText) {
        progressActivityId = await updateOrPost(port, progressActivityId, fullText);
        flushedText = fullText;
      }
      return { progressActivityId };
    },
    async complete(finalText, fullText) {
      const text = finalText || fullText;
      if (text !== flushedText) {
        progressActivityId = await updateOrPost(port, progressActivityId, text);
        flushedText = text;
      }
      return { progressActivityId };
    },
    async fail(text) {
      progressActivityId = await updateOrPost(port, progressActivityId, text);
      flushedText = text;
      return { progressActivityId };
    },
  };
}

async function updateOrPost(
  port: {
    post(text: string): Promise<{ id?: string } | unknown>;
    update(messageId: string, text: string): Promise<unknown>;
  },
  messageId: string | undefined,
  text: string,
): Promise<string | undefined> {
  if (!messageId) {
    return activityId(await port.post(text));
  }
  try {
    await port.update(messageId, text);
    return messageId;
  } catch {
    return activityId(await port.post(text));
  }
}

function activityId(value: unknown): string | undefined {
  return typeof value === 'object' && value !== null && 'id' in value
    ? String((value as { id?: unknown }).id ?? '') || undefined
    : undefined;
}
