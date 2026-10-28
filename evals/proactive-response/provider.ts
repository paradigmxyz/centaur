// Runs the candidate proactive-response classifier on a case's thread. Each
// `thread` entry is a one-key map from author to text, oldest first; the last
// entry is the newest message and the author `centaur` is the bot. History is
// built as slackbotv2 builds it: the latest earlier messages, bot replies
// included, oldest first.
import type { ApiProvider, CallApiContextParams, ProviderOptions, ProviderResponse } from 'promptfoo'
import { classify, type ThreadMessage } from './classifier'

const BOT_NAME = 'centaur'
// slackbotv2's PROACTIVE_HISTORY_MESSAGES.
const MAX_HISTORY_MESSAGES = 20

type Config = {
  model?: string
  timeoutMs?: number
}

export default class ProactiveResponseProvider implements ApiProvider {
  readonly config: Config
  private readonly providerId: string

  constructor(options: ProviderOptions) {
    this.config = (options.config ?? {}) as Config
    this.providerId = options.id ?? `proactive-response:${this.config.model}`
  }

  id(): string {
    return this.providerId
  }

  async callApi(_prompt: string, context?: CallApiContextParams): Promise<ProviderResponse> {
    const apiKey = process.env.OPENAI_API_KEY
    if (!apiKey) return { error: 'OPENAI_API_KEY is not set' }

    let messages: ThreadMessage[]
    try {
      messages = threadMessages(context?.vars.thread)
    } catch (error) {
      return { error: (error as Error).message }
    }

    const tokenUsage = { completion: 0, prompt: 0, total: 0 }
    try {
      const decision = await classify(
        {
          botName: BOT_NAME,
          history: messages.slice(0, -1).slice(-MAX_HISTORY_MESSAGES),
          message: messages.at(-1)!
        },
        {
          apiKey,
          baseUrl: process.env.OPENAI_BASE_URL,
          fetch: (async (input: RequestInfo | URL, init?: RequestInit) => {
            const response = await fetch(input, init)
            const usage = response.ok ? (await response.clone().json()).usage : undefined
            tokenUsage.prompt += usage?.input_tokens ?? 0
            tokenUsage.completion += usage?.output_tokens ?? 0
            tokenUsage.total += usage?.total_tokens ?? 0
            return response
          }) as typeof fetch,
          model: this.config.model ?? 'gpt-6-luna'
        },
        AbortSignal.timeout(this.config.timeoutMs ?? 10_000)
      )
      return { output: decision, tokenUsage }
    } catch (error) {
      return { error: (error as Error).message }
    }
  }
}

function threadMessages(thread: unknown): ThreadMessage[] {
  if (!Array.isArray(thread) || thread.length === 0) {
    throw new Error('thread must be a non-empty list of {author: text} entries')
  }
  return thread.map(entry => {
    const pairs = entry && typeof entry === 'object' ? Object.entries(entry) : []
    const [author, text] = pairs[0] ?? []
    if (pairs.length !== 1 || typeof text !== 'string') {
      throw new Error(`thread entry must be a single {author: text} pair: ${JSON.stringify(entry)}`)
    }
    return { author, fromBot: author === BOT_NAME, text }
  })
}
