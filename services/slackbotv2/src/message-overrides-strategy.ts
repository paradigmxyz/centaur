import type { Logger } from 'chat'
import {
  extractMessageOverrides,
  validateStrategyOverrides
} from './overrides'
import type { JsonObject, MessageOverridesStrategy } from './types'
import { errorMessage, isJsonObject } from './utils'

const DEFAULT_TIMEOUT_MS = 2_000
const DEFAULT_MAX_OUTPUT_TOKENS = 300

const SYSTEM_PROMPT = [
  'Decide whether the Slack message asks to use a specific AI harness, model, provider, or reasoning effort.',
  'Return only canonical override values from the schema.',
  'Use null for every field when the message does not ask to change model selection.',
  'Allowed harness values: codex, claudecode, amp, nanocodex.',
  'Allowed provider values: responses, amazon-bedrock, openrouter.',
  'Allowed reasoning values: none, minimal, low, medium, high, xhigh, max.',
  'Treat inline flags such as "--claude", "--claude --model=fable", and "--fable" as model selection requests.',
  'In this Slackbot, a request to use Claude without another named Claude model means harness claudecode and model claude-opus-4-8. Examples: "--claude what model are you?" and "using claude:" select harness claudecode and model claude-opus-4-8. Explicit Fable requests such as "--claude --model=fable" and "using claude fable:" select harness claudecode and model claude-fable-5.',
  'Map fuzzy effort words to the nearest reasoning value by magnitude. Examples: tiny/cheap/fast -> low or minimal; normal/default -> medium; deep/strong/intense -> high or xhigh; maximum/superduper/biggest -> max.',
  'Return reasoning even when the requested model is not Codex; validation will ignore reasoning that cannot apply.',
  'Map OpenAI model aliases to canonical IDs: sol -> gpt-5.6-sol, terra -> gpt-5.6-terra, luna -> gpt-5.6-luna, 5.5 -> gpt-5.5, 5.5 pro -> gpt-5.5-pro, 5.4 -> gpt-5.4, 5.4 pro -> gpt-5.4-pro, 5.4 mini -> gpt-5.4-mini, 5.4 nano -> gpt-5.4-nano.',
  'Map Claude model aliases to canonical IDs: fable -> claude-fable-5, opus -> claude-opus-4-8, opus 4.7 -> claude-opus-4-7, sonnet -> claude-sonnet-4-6, sonnet 5 -> claude-sonnet-5, haiku -> claude-haiku-4-5.',
  'Map Amp model aliases to canonical IDs: deep -> deep, fast -> fast.',
  'For example, "use max effort and the sol model" should return model "gpt-5.6-sol" and reasoning "max".',
  'Do not treat ordinary discussion of model names as a selection request.'
].join('\n')

const MODEL_VALUES = [
  'claude-fable-5',
  'claude-haiku-4-5',
  'claude-opus-4-7',
  'claude-opus-4-8',
  'claude-sonnet-4-6',
  'claude-sonnet-5',
  'deep',
  'fast',
  'gpt-5.4',
  'gpt-5.4-mini',
  'gpt-5.4-nano',
  'gpt-5.4-pro',
  'gpt-5.5',
  'gpt-5.5-pro',
  'gpt-5.6-luna',
  'gpt-5.6-sol',
  'gpt-5.6-terra',
  null
] as const

const MESSAGE_OVERRIDES_SCHEMA = {
  additionalProperties: false,
  properties: {
    harness: {
      enum: ['codex', 'claudecode', 'amp', 'nanocodex', null],
      type: ['string', 'null']
    },
    model: {
      enum: MODEL_VALUES,
      type: ['string', 'null']
    },
    provider: {
      enum: ['responses', 'amazon-bedrock', 'openrouter', null],
      type: ['string', 'null']
    },
    reasoning: {
      enum: ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', null],
      type: ['string', 'null']
    }
  },
  required: ['harness', 'model', 'provider', 'reasoning'],
  type: 'object'
}

export type OpenAiMessageOverridesStrategyOptions = {
  apiKey: string
  baseUrl?: string
  fetch?: typeof fetch
  logger?: Logger
  maxOutputTokens?: number
  model: string
  timeoutMs?: number
}

type OpenAiMessageOverridesStrategyOutput = {
  harness?: unknown
  model?: unknown
  provider?: unknown
  reasoning?: unknown
}

export function createFlagMessageOverridesStrategy(): MessageOverridesStrategy {
  return async ({ text }) => {
    const parsed = extractMessageOverrides(text)
    const { cleanedText, ...overrides } = parsed
    return { cleanedText, overrides }
  }
}

export function createOpenAiMessageOverridesStrategy(
  options: OpenAiMessageOverridesStrategyOptions
): MessageOverridesStrategy {
  const responsesUrl = `${(options.baseUrl ?? 'https://api.openai.com/v1').replace(/\/+$/, '')}/responses`
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
  const maxOutputTokens = options.maxOutputTokens ?? DEFAULT_MAX_OUTPUT_TOKENS
  const fetchFn = options.fetch ?? fetch

  return async ({ text }) => {
    // Explicit flags are a deterministic user command, even when the deployment
    // enables the LLM strategy for natural-language model requests. Handle them
    // first so a strict strategy schema or model failure cannot discard the
    // selection, and so flags never leak into the harness prompt.
    const { cleanedText, ...explicitOverrides } = extractMessageOverrides(text)
    if (Object.values(explicitOverrides).some(value => value !== undefined)) {
      return { cleanedText, overrides: explicitOverrides }
    }

    const controller = new AbortController()
    const timeout = setTimeout(() => controller.abort(), timeoutMs)
    try {
      const response = await fetchFn(responsesUrl, {
        body: JSON.stringify({
          input: text,
          instructions: SYSTEM_PROMPT,
          max_output_tokens: maxOutputTokens,
          model: options.model,
          reasoning: { effort: 'none' },
          store: false,
          text: {
            format: {
              name: 'slack_message_overrides',
              schema: MESSAGE_OVERRIDES_SCHEMA,
              strict: true,
              type: 'json_schema'
            }
          }
        }),
        headers: {
          authorization: `Bearer ${options.apiKey}`,
          'content-type': 'application/json'
        },
        method: 'POST',
        signal: controller.signal
      })
      if (!response.ok) {
        throw new Error(
          `message overrides strategy request failed with HTTP ${response.status} ${response.statusText}`
        )
      }
      const value = await response.json()
      const outputText = responseOutputText(value)
      options.logger?.info('slackbotv2_message_overrides_strategy_response_received', {
        model: options.model,
        output_text: outputText
      })
      if (!outputText) {
        throw new Error('message overrides strategy response did not include output text')
      }
      const parsed = JSON.parse(outputText)
      return {
        overrides: validateStrategyOverrides(
          isJsonObject(parsed) ? (parsed as OpenAiMessageOverridesStrategyOutput) : null
        )
      }
    } catch (error) {
      options.logger?.warn('slackbotv2_message_overrides_strategy_request_failed', {
        error: errorMessage(error),
        model: options.model,
        timeout_ms: timeoutMs
      })
      return { overrides: {} }
    } finally {
      clearTimeout(timeout)
    }
  }
}

function responseOutputText(value: unknown): string | undefined {
  const parts = arrayValue(isJsonObject(value) ? value.output : undefined).flatMap(item =>
    arrayValue(isJsonObject(item) ? item.content : undefined).flatMap(content =>
      isJsonObject(content) && typeof content.text === 'string' ? [content.text] : []
    )
  )
  return parts.length > 0 ? parts.join('\n') : undefined
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : []
}
