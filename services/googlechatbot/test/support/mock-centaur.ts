import type { FetchFn } from '../../src/types.js';

export type MockCentaurRequest = {
  body?: unknown;
  method: string;
  path: string;
};

export function createMockCentaurFetch(
  answer = 'PONG',
  chunks: string[] = [answer],
  options: { chunkDelayMs?: number } = {},
): {
  fetch: FetchFn;
  requests: MockCentaurRequest[];
} {
  const requests: MockCentaurRequest[] = [];
  const mockFetch: FetchFn = async (input, init) => {
    const url = new URL(String(input));
    const path = url.pathname;
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    requests.push({ body, method: init?.method ?? 'GET', path });

    if (path.endsWith('/events')) {
      return new Response(sseStream(answer, chunks, options), {
        headers: { 'content-type': 'text/event-stream' },
        status: 200,
      });
    }

    if (path.endsWith('/execute')) {
      const threadKey = decodeURIComponent(path.split('/').at(-2) ?? 'thread');
      return Response.json({
        execution_id: 'exec-1',
        ok: true,
        status: 'running',
        thread_key: threadKey,
      });
    }

    return Response.json({ ok: true });
  };

  return { fetch: mockFetch, requests };
}

function sseStream(
  answer: string,
  chunks: string[],
  options: { chunkDelayMs?: number },
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    async start(controller) {
      controller.enqueue(encoder.encode(outputEvent(1, {
        method: 'item/started',
        params: {
          itemId: 'answer-1',
          item: { id: 'answer-1', type: 'agentMessage', phase: 'final_answer', text: '' },
        },
      })));

      for (const [index, chunk] of chunks.entries()) {
        controller.enqueue(encoder.encode(outputEvent(index + 2, {
          method: 'item/agentMessage/delta',
          params: { itemId: 'answer-1', delta: chunk },
        })));
        if (options.chunkDelayMs) {
          await new Promise((resolve) => setTimeout(resolve, options.chunkDelayMs));
        }
      }

      controller.enqueue(encoder.encode(outputEvent(chunks.length + 2, {
        method: 'item/completed',
        params: {
          itemId: 'answer-1',
          item: { id: 'answer-1', type: 'agentMessage', phase: 'final_answer', text: answer },
        },
      })));
      controller.enqueue(encoder.encode(`id: ${chunks.length + 3}\nevent: session.execution_completed\ndata: {}\n\n`));
      controller.close();
    },
  });
}

function outputEvent(id: number, payload: unknown): string {
  return `id: ${id}\nevent: session.output.line\ndata: ${JSON.stringify(payload)}\n\n`;
}
