export type TeamsRenderChunk =
  | { type: 'text_delta'; text: string }
  | { type: 'done' }
  | { type: 'error'; error: string };

export async function* conflateTeamsRenderStream(
  source: AsyncIterable<TeamsRenderChunk>,
): AsyncIterable<TeamsRenderChunk> {
  const iterator = source[Symbol.asyncIterator]();
  let pendingText = '';
  let pendingTerminal: Extract<TeamsRenderChunk, { type: 'done' | 'error' }> | undefined;
  let sourceDone = false;
  let sourceFailed = false;
  let sourceError: unknown;
  let aborted = false;
  let wake: (() => void) | undefined;

  const pump = (async () => {
    try {
      while (!aborted) {
        const result = await iterator.next();
        if (result.done) {
          return;
        }
        const chunk = result.value;
        if (chunk.type === 'text_delta') {
          pendingText += chunk.text;
        } else {
          pendingTerminal = chunk;
        }
        wake?.();
      }
    } catch (error) {
      sourceFailed = true;
      sourceError = error;
    } finally {
      sourceDone = true;
      wake?.();
    }
  })();

  try {
    while (true) {
      if (pendingText) {
        const text = pendingText;
        pendingText = '';
        yield { type: 'text_delta', text };
        continue;
      }
      if (pendingTerminal) {
        const terminal = pendingTerminal;
        pendingTerminal = undefined;
        yield terminal;
        continue;
      }
      if (sourceFailed) {
        throw sourceError;
      }
      if (sourceDone) {
        return;
      }
      await new Promise<void>((resolve) => {
        wake = resolve;
      });
      wake = undefined;
    }
  } finally {
    aborted = true;
    wake = undefined;
    void pump.catch(() => undefined);
    if (!sourceDone) {
      void Promise.resolve(iterator.return?.()).catch(() => undefined);
    }
  }
}
