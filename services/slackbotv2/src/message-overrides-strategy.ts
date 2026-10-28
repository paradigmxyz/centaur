import type { Logger } from 'chat'
import {
  extractMessageOverrides,
  validateStrategyOverrides
} from './overrides'
import type { MessageOverridesStrategy } from './types'
import { errorMessage, isJsonObject } from './utils'

const DEFAULT_TIMEOUT_MS = 2_000
// Minimum probability that the message asks for a selection at all. Below it,
// every per-field answer is ignored.
const SELECTION_PROBABILITY_THRESHOLD = 0.5
const UNSPECIFIED = 'unspecified'

const INSTRUCTIONS = [
  'You evaluate a Slack message sent to an AI coding bot and decide whether it asks to choose the AI harness, model, provider, or reasoning effort used to answer it.',
  'A selection is an explicit instruction to the bot about how to run, such as "use X", "switch to X", "run this on X", "with high effort", or a leading "using X:" prefix.',
  '"using claude:" or "use claude" with no other Claude model named selects the default Claude model, Opus 5.5.',
  'Only explicit requests count. Words in quoted text, pasted output, code, config values like reasoning=high, task requirements, questions about the bot, or ordinary discussion of model names are not selection requests.',
  'Words merely evoking speed or depth, such as "think deeply", "deep analysis", "quick question", or "a fast answer", select neither a model nor a reasoning effort.',
  'Reasoning effort is selected only when the user explicitly mentions reasoning, effort, or thinking level; then map magnitude words to the nearest level.'
].join('\n')

// Only messages that mention a model name or selection term can ask for an
// override, so other messages skip the classifier and its latency. The letter
// lookarounds keep version suffixes ("gpt5", "opus-4.7") and plurals matching
// while ignoring longer words such as "solve" or "terraform".
const SELECTOR_TERM_PATTERN =
  /(?<![a-z])(?:claude|claudecode|codex|amp|nanocodex|omp|opus|sonnet|haiku|fable|gpt|astra|sol|luna|terra|model|harness|provider|reasoning|effort|mode|bedrock|openrouter|think|thinking)(?:s|es)?(?![a-z])/i

type Choice = { description?: string; value: string }

const HARNESS_CHOICES: Choice[] = ['codex', 'claudecode', 'amp', 'nanocodex', 'pi', 'omp'].map(
  value => ({ value })
)

const PROVIDER_CHOICES: Choice[] = ['responses', 'amazon-bedrock', 'openrouter'].map(value => ({
  value
}))

const REASONING_CHOICES: Choice[] = [
  { value: 'none', description: 'No reasoning.' },
  { value: 'minimal', description: 'Minimal; tiny.' },
  { value: 'low', description: 'Low; cheap, fast, quick.' },
  { value: 'medium', description: 'Medium; normal, default.' },
  { value: 'high', description: 'High; deep, strong, hard.' },
  { value: 'xhigh', description: 'Extra high; intense, very hard.' },
  { value: 'max', description: 'Max; maximum, superduper, biggest, hardest.' },
  { value: 'ultra', description: 'Ultra.' }
]

// Descriptions carry the user-facing aliases. Each states exactly when it
// applies so a bare family name ("sol") resolves to its default generation.
const MODEL_CHOICES: Choice[] = [
  { value: 'claude-fable-5', description: 'Only when the user says "fable 5" (not 5.1).' },
  { value: 'claude-fable-5-1', description: 'Fable: plain "fable", "claude fable", or "fable 5.1".' },
  { value: 'claude-haiku-4-5', description: 'Only when the user says "haiku 4.5".' },
  { value: 'claude-haiku-5-5', description: 'Haiku: plain "haiku" or "haiku 5.5".' },
  { value: 'claude-opus-4-7', description: 'Only when the user says "opus 4.7".' },
  { value: 'claude-opus-4-8', description: 'Only when the user says "opus 4.8".' },
  { value: 'claude-opus-5', description: 'Only when the user says "opus 5" (not 5.5, not fast).' },
  { value: 'claude-opus-5-fast', description: 'Only when the user says "opus 5 fast".' },
  {
    value: 'claude-opus-5-5',
    description:
      'Default Claude model: plain "opus", "opus 5.5", or "claude" / "using claude:" with no other Claude model named.'
  },
  { value: 'claude-sonnet-4-6', description: 'Only when the user says "sonnet 4.6".' },
  { value: 'claude-sonnet-5', description: 'Only when the user says "sonnet 5" (not 5.5).' },
  { value: 'claude-sonnet-5-5', description: 'Sonnet: plain "sonnet" or "sonnet 5.5".' },
  {
    value: 'deep',
    description: 'Amp deep model. Only when Amp is named or the user asks for "the deep model/mode".'
  },
  {
    value: 'fast',
    description: 'Amp fast model. Only when Amp is named or the user asks for "the fast model/mode".'
  },
  { value: 'gpt-5.4', description: 'Only when the user says "5.4" with no suffix.' },
  { value: 'gpt-5.4-mini', description: 'Only when the user says "5.4 mini".' },
  { value: 'gpt-5.4-nano', description: 'Only when the user says "5.4 nano".' },
  { value: 'gpt-5.4-pro', description: 'Only when the user says "5.4 pro".' },
  { value: 'gpt-5.5', description: 'Only when the user says "5.5" with no suffix.' },
  { value: 'gpt-5.5-pro', description: 'Only when the user says "5.5 pro".' },
  { value: 'gpt-5.6-luna', description: 'Only when the user says "5.6 luna".' },
  { value: 'gpt-5.6-sol', description: 'Only when the user says "5.6 sol".' },
  { value: 'gpt-5.6-terra', description: 'Only when the user says "5.6 terra".' },
  { value: 'gpt-6-astra', description: 'Astra: plain "astra".' },
  { value: 'gpt-6-sol', description: 'Only when the user says "6 sol" (not 6.1).' },
  { value: 'gpt-6.1-sol', description: 'Sol: plain "sol", "the sol model", or "6.1 sol".' },
  { value: 'gpt-6.1-terra', description: 'Terra: plain "terra" or "6.1 terra".' },
  { value: 'gpt-6-luna', description: 'Luna: plain "luna" or "6 luna".' }
]

function choiceQuestion(name: string, instructions: string, choices: Choice[]) {
  return {
    choices: [
      { value: UNSPECIFIED, description: 'The message does not explicitly request this.' },
      ...choices
    ],
    instructions,
    name,
    type: 'choice'
  }
}

const QUESTIONS = [
  {
    instructions:
      'Does the message explicitly ask to change the AI harness, model, provider, or reasoning effort used to answer it?',
    name: 'selection_request',
    type: 'predicate'
  },
  choiceQuestion('harness', 'Which AI harness does the message explicitly request?', HARNESS_CHOICES),
  choiceQuestion(
    'model',
    'Which AI model does the message explicitly request? Preserve the requested generation.',
    MODEL_CHOICES
  ),
  choiceQuestion(
    'provider',
    'Which inference provider does the message explicitly request?',
    PROVIDER_CHOICES
  ),
  choiceQuestion(
    'reasoning',
    'Which reasoning effort does the message explicitly request? Map magnitude words to the nearest level.',
    REASONING_CHOICES
  )
]

export type OpenAiMessageOverridesStrategyOptions = {
  apiKey: string
  baseUrl?: string
  fetch?: typeof fetch
  logger?: Logger
  model: string
  timeoutMs?: number
}

export function createFlagMessageOverridesStrategy(): MessageOverridesStrategy {
  return async ({ text }) => {
    const parsed = extractMessageOverrides(text)
    const { cleanedText, ...overrides } = parsed
    return { cleanedText, overrides }
  }
}

/** Classifies natural-language model selection with the OpenAI Decisions API. */
export function createOpenAiMessageOverridesStrategy(
  options: OpenAiMessageOverridesStrategyOptions
): MessageOverridesStrategy {
  const decisionsUrl = `${(options.baseUrl ?? 'https://api.openai.com/v1').replace(/\/+$/, '')}/decisions`
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
  const fetchFn = options.fetch ?? fetch

  return async ({ text }) => {
    // Explicit model flags are deterministic user commands and bypass the
    // strategy entirely. The wrapper has already removed any persona flag.
    const { cleanedText, ...explicitOverrides } = extractMessageOverrides(text)
    if (Object.values(explicitOverrides).some(value => value !== undefined)) {
      return { cleanedText, overrides: explicitOverrides }
    }

    const strategyText = cleanedText
    if (!strategyText) {
      return { cleanedText, overrides: {} }
    }
    if (!SELECTOR_TERM_PATTERN.test(strategyText)) {
      return { overrides: {} }
    }

    const startedAtMs = Date.now()
    const controller = new AbortController()
    const timeout = setTimeout(() => controller.abort(), timeoutMs)
    try {
      const response = await fetchFn(decisionsUrl, {
        body: JSON.stringify({
          input: [
            { content: INSTRUCTIONS, role: 'developer' },
            { content: strategyText, role: 'user' }
          ],
          model: options.model,
          questions: QUESTIONS
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
      const answers = decisionAnswers(await response.json())
      const selectionProbability = answers.selection_request?.probability
      const selection = {
        harness: chosenValue(answers.harness),
        model: chosenValue(answers.model),
        provider: chosenValue(answers.provider),
        reasoning: chosenValue(answers.reasoning)
      }
      options.logger?.info('slackbotv2_message_overrides_strategy_response_received', {
        elapsed_ms: Date.now() - startedAtMs,
        model: options.model,
        selected_harness: selection.harness,
        selected_model: selection.model,
        selected_provider: selection.provider,
        selected_reasoning: selection.reasoning,
        selection_probability: selectionProbability
      })
      if (
        typeof selectionProbability !== 'number' ||
        selectionProbability < SELECTION_PROBABILITY_THRESHOLD
      ) {
        return { overrides: {} }
      }
      return { overrides: validateStrategyOverrides(selection) }
    } catch (error) {
      options.logger?.warn('slackbotv2_message_overrides_strategy_request_failed', {
        elapsed_ms: Date.now() - startedAtMs,
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

type DecisionAnswer = { choice?: unknown; probability?: unknown; type?: unknown }

function decisionAnswers(value: unknown): Record<string, DecisionAnswer> {
  const answers = isJsonObject(value) && Array.isArray(value.answers) ? value.answers : []
  return Object.fromEntries(
    answers.flatMap(answer =>
      isJsonObject(answer) && typeof answer.name === 'string' ? [[answer.name, answer]] : []
    )
  )
}

// Refusals and the unspecified sentinel both mean "no selection" for a field.
function chosenValue(answer: DecisionAnswer | undefined): string | null {
  return answer?.type === 'choice' &&
    typeof answer.choice === 'string' &&
    answer.choice !== UNSPECIFIED
    ? answer.choice
    : null
}
