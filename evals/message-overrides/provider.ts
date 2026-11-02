// Runs the production Slack message-overrides classifier so the eval measures
// the Decisions instructions and questions, selector pre-filter, and validation
// the bot ships.
import type { ApiProvider, ProviderOptions, ProviderResponse } from 'promptfoo'
import { createOpenAiMessageOverridesStrategy } from '../../services/slackbotv2/src/message-overrides-strategy'

type Config = {
  model?: string
  timeoutMs?: number
}

export default class MessageOverridesProvider implements ApiProvider {
  readonly config: Config
  private readonly providerId: string

  constructor(options: ProviderOptions) {
    this.config = (options.config ?? {}) as Config
    this.providerId = options.id ?? `message-overrides:${this.config.model}`
  }

  id(): string {
    return this.providerId
  }

  async callApi(text: string): Promise<ProviderResponse> {
    const apiKey = process.env.OPENAI_API_KEY
    if (!apiKey) return { error: 'OPENAI_API_KEY is not set' }

    // The strategy degrades to "no override" on failure; record its warning so
    // request errors and timeouts fail the case instead of scoring as negatives.
    let failure: string | undefined
    const tokenUsage = { completion: 0, prompt: 0, total: 0 }
    const strategy = createOpenAiMessageOverridesStrategy({
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
      logger: {
        child: function () {
          return this
        },
        debug: () => undefined,
        error: () => undefined,
        info: () => undefined,
        warn: (message: string, fields?: { error?: string }) => {
          failure = `${message}: ${fields?.error ?? 'unknown error'}`
        }
      },
      model: this.config.model ?? 'gpt-6-luna',
      timeoutMs: this.config.timeoutMs
    })

    const { overrides } = await strategy({ text })
    if (failure) return { error: failure }
    return {
      output: {
        harness: overrides.harnessType ?? null,
        model: overrides.model ?? null,
        provider: overrides.provider ?? null,
        reasoning: overrides.reasoning ?? null
      },
      tokenUsage
    }
  }
}
