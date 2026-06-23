import type { ChatSDKStreamChunk } from '@centaur/rendering'

type TaskChunk = Extract<ChatSDKStreamChunk, { type: 'task_update' }>
type PlanChunk = Extract<ChatSDKStreamChunk, { type: 'plan_update' }>

/**
 * Conflates a Chat SDK chunk stream for a slow consumer.
 *
 * Slack rendering pays one rate-limited API call per chunk, while a busy
 * execution can emit tens of thousands of chunks. Without conflation the
 * consumer replays every intermediate state and a large turn can take longer
 * to render than to run. This wrapper drains the source eagerly into a
 * pending snapshot while the consumer is busy:
 *
 * - `markdown_text` deltas are concatenated into one pending chunk (markdown
 *   is append-only content, so merging loses nothing),
 * - `task_update`s are keyed by id and merged per field, newest value
 *   winning. Updates often omit `details`/`output` to mean "unchanged" (the
 *   chunk schema cannot express clearing a field), so absent fields inherit
 *   the pending value instead of dropping content the consumer never sent,
 * - `plan_update` keeps only the latest title.
 *
 * Each consumer pull yields one pending item (plan first, then tasks in
 * first-seen order, then markdown), so the Slack call count is bounded by
 * distinct cards plus markdown volume instead of by source event count.
 * When the consumer keeps up, pending holds at most one item and the stream
 * behaves exactly like the unwrapped source.
 */
export async function* conflateChatSdkStream(
  source: AsyncIterable<ChatSDKStreamChunk>
): AsyncIterable<ChatSDKStreamChunk> {
  const iterator = source[Symbol.asyncIterator]()
  // Map.set on an existing key keeps its insertion position, so cards stay
  // in first-seen order even when they update while pending.
  const pendingTasks = new Map<string, TaskChunk>()
  let pendingPlan: PlanChunk | undefined
  let pendingMarkdown = ''
  let sourceDone = false
  let sourceFailed = false
  let sourceError: unknown
  let aborted = false
  let wake: (() => void) | undefined

  const pump = (async () => {
    try {
      while (!aborted) {
        const result = await iterator.next()
        if (result.done) return
        const chunk = result.value
        if (chunk.type === 'markdown_text') {
          pendingMarkdown += chunk.text
        } else if (chunk.type === 'plan_update') {
          pendingPlan = chunk
        } else {
          const pending = pendingTasks.get(chunk.id)
          pendingTasks.set(chunk.id, pending ? { ...pending, ...chunk } : chunk)
        }
        wake?.()
      }
    } catch (error) {
      sourceFailed = true
      sourceError = error
    } finally {
      sourceDone = true
      wake?.()
    }
  })()

  try {
    while (true) {
      if (pendingPlan) {
        const plan = pendingPlan
        pendingPlan = undefined
        yield plan
        continue
      }
      const nextTask = pendingTasks.entries().next()
      if (!nextTask.done) {
        const [id, task] = nextTask.value
        pendingTasks.delete(id)
        yield task
        continue
      }
      if (pendingMarkdown) {
        const text = pendingMarkdown
        pendingMarkdown = ''
        yield { type: 'markdown_text', text }
        continue
      }
      // Run-to-completion guarantees the pump cannot fold between the checks
      // above and the await below, so no wakeup can be lost.
      if (sourceFailed) throw sourceError
      if (sourceDone) return
      await new Promise<void>(resolve => {
        wake = resolve
      })
      wake = undefined
    }
  } finally {
    // Consumer finished or abandoned the stream: stop pumping and cancel the
    // source so a live SSE is not held open. Do not await either - a silent
    // source can keep a pending next() unsettled indefinitely.
    aborted = true
    wake = undefined
    void pump.catch(() => undefined)
    if (!sourceDone) {
      void Promise.resolve(iterator.return?.()).catch(() => undefined)
    }
  }
}
