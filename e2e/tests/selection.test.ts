// Harness, model, reasoning, and persona selection across a Slack thread,
// following the rules in services/slackbotv2/src/overrides.ts and
// channel-defaults.ts: a harness flag picks the harness and resets the model;
// harness and model stick to the thread until another explicit flag moves
// them; reasoning effort applies to one turn; the persona is pinned when the
// thread's session is created; a channel default sits below every flag; flags
// never reach the model. Each step checks what actually ran: the sandbox's
// harness, and the model, effort, and persona prompt the provider was given.
import { expect, test } from 'bun:test'
import claudeSettings from '../../harness/claude/settings.json'
import codexConfig from '../../harness/codex/config.toml'
import {
  CHANNEL,
  DEFAULTS_CHANNEL,
  model,
  slack,
  turnTimeoutMs,
  type Channel,
  type ModelRequest,
  type Thread,
  type Turn
} from '../lib'

const CODEX_MODEL = (codexConfig as { model: string }).model
const CODEX_EFFORT = (codexConfig as { model_reasoning_effort: string }).model_reasoning_effort
const CLAUDE_MODEL = claudeSettings.model
// Claude Code picks its default effort per model; this is its choice for the
// baked model. Steps on other Claude models leave effort unchecked.
const CLAUDE_EFFORT = 'medium'
// Deployed personas, each recognized by the first line of its prompt.
const PERSONA_HEADINGS: Record<string, string> = {
  eng: (await Bun.file(new URL('../../tools/personas/eng/PROMPT.md', import.meta.url)).text()).split('\n')[0]!
}

type Step = {
  say: string
  harness: 'codex' | 'claudecode'
  model: string
  /** Effort the provider was asked for; omitted when the harness picks it. */
  effort?: string
  /** Whether this turn ran in the previous turn's sandbox. Omitted on the first turn. */
  sandbox?: 'same' | 'new'
  /** Earlier answers the harness must still hold in its own session this turn. */
  sees?: number[]
  /** Persona whose prompt the model was given; omitted when none. */
  persona?: string
  /** Warning shown under the reply; omitted when none. */
  notice?: string
}

type Scenario = {
  /** Where the thread runs; DEFAULTS_CHANNEL carries the channel defaults in e2e/infra/values.yaml. */
  channel?: Channel
  steps: Step[]
}

const scenarios: Record<string, Scenario> = {
  'no flag runs the deployment default': {
    steps: [{ say: 'hello', harness: 'codex', model: CODEX_MODEL, effort: CODEX_EFFORT }]
  },
  'a harness flag sticks to the thread': {
    steps: [
      { say: '--claude hello', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT },
      { say: 'again', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, sandbox: 'same', sees: [1] }
    ]
  },
  'a harness flag on a follow-up switches the thread': {
    steps: [
      { say: '--codex hello', harness: 'codex', model: CODEX_MODEL, effort: CODEX_EFFORT },
      { say: '--claude switch', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, sandbox: 'new' },
      { say: 'again', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, sandbox: 'same', sees: [2] }
    ]
  },
  'a model flag sticks to the thread': {
    steps: [
      { say: '--codex --model gpt-5.4 hello', harness: 'codex', model: 'gpt-5.4', effort: CODEX_EFFORT },
      { say: 'again', harness: 'codex', model: 'gpt-5.4', effort: CODEX_EFFORT, sandbox: 'same' }
    ]
  },
  'a harness switch drops the sticky model': {
    steps: [
      { say: '--codex --model gpt-5.4 hello', harness: 'codex', model: 'gpt-5.4', effort: CODEX_EFFORT },
      { say: '--claude switch', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, sandbox: 'new' },
      { say: '--codex back', harness: 'codex', model: CODEX_MODEL, effort: CODEX_EFFORT, sandbox: 'new' }
    ]
  },
  'a Claude model shortcut picks the harness and model': {
    steps: [
      { say: '--sonnet hello', harness: 'claudecode', model: 'claude-sonnet-5' },
      { say: 'again', harness: 'claudecode', model: 'claude-sonnet-5', sandbox: 'same' }
    ]
  },
  'a Claude alias works as a --model value': {
    steps: [{ say: '--claude --model sonnet hello', harness: 'claudecode', model: 'claude-sonnet-5' }]
  },
  'claude reasoning effort applies to one turn': {
    steps: [
      { say: '--claude -rsn low hello', harness: 'claudecode', model: CLAUDE_MODEL, effort: 'low' },
      { say: 'again', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, sandbox: 'same' }
    ]
  },
  'an effort the model does not support is not sent': {
    steps: [
      { say: '--codex --model gpt-5.4-pro -rsn low hello', harness: 'codex', model: 'gpt-5.4-pro', effort: CODEX_EFFORT }
    ]
  },
  'a persona flag sticks to the thread alongside a harness flag': {
    steps: [
      { say: '--claude --persona=eng hello', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, persona: 'eng' },
      { say: 'again', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, persona: 'eng', sandbox: 'same', sees: [1] }
    ]
  },
  'a later persona flag does not move the pinned persona': {
    steps: [
      { say: '--codex --persona=eng hello', harness: 'codex', model: CODEX_MODEL, effort: CODEX_EFFORT, persona: 'eng' },
      { say: '--persona=missing again', harness: 'codex', model: CODEX_MODEL, effort: CODEX_EFFORT, persona: 'eng', sandbox: 'same' }
    ]
  },
  'a harness switch keeps the pinned persona': {
    steps: [
      { say: '--codex --persona=eng hello', harness: 'codex', model: CODEX_MODEL, effort: CODEX_EFFORT, persona: 'eng' },
      { say: '--claude switch', harness: 'claudecode', model: CLAUDE_MODEL, effort: CLAUDE_EFFORT, persona: 'eng', sandbox: 'new' }
    ]
  },
  'an unavailable persona continues without one': {
    steps: [
      {
        say: '--codex --persona=missing hello',
        harness: 'codex',
        model: CODEX_MODEL,
        effort: CODEX_EFFORT,
        notice: 'Persona "missing" isn\'t available. Continuing without a persona.'
      }
    ]
  },
  'a channel default picks the persona, harness, and model': {
    channel: DEFAULTS_CHANNEL,
    steps: [
      { say: 'hello', harness: 'claudecode', model: 'claude-sonnet-5', persona: 'eng' },
      { say: 'again', harness: 'claudecode', model: 'claude-sonnet-5', persona: 'eng', sandbox: 'same', sees: [1] }
    ]
  },
  'flags beat the channel default': {
    channel: DEFAULTS_CHANNEL,
    steps: [
      { say: '--codex --model gpt-5.4 -rsn low hello', harness: 'codex', model: 'gpt-5.4', effort: 'low', persona: 'eng' }
    ]
  },
  'a harness flag does not carry the channel model to the new harness': {
    channel: DEFAULTS_CHANNEL,
    steps: [
      // The channel's reasoning default still applies to every turn.
      { say: '--codex hello', harness: 'codex', model: CODEX_MODEL, effort: 'high', persona: 'eng' },
      { say: 'again', harness: 'codex', model: CODEX_MODEL, effort: 'high', persona: 'eng', sandbox: 'same' }
    ]
  }
}

function personaIn(request: ModelRequest | undefined): string | undefined {
  return Object.entries(PERSONA_HEADINGS).find(([, heading]) => request?.prompt.includes(heading))?.[0]
}

/** The warning segment of a reply's context line, as slackbotv2 renders it. */
function noticeIn(context: string): string | undefined {
  return context.match(/:warning: (.+?)(?: · |$)/)?.[1]
}

for (const [name, scenario] of Object.entries(scenarios)) {
  test.concurrent(name, async () => {
    let thread: Thread | undefined
    const turns: Turn[] = []
    for (const [index, step] of scenario.steps.entries()) {
      const answer = model.says(`Answer ${index + 1}.`)
      if (thread) await thread.mention(step.say, answer)
      else thread = await slack.mention(step.say, answer, scenario.channel ?? CHANNEL)
      const turn = await thread.nextTurn()
      const request = turn.request
      // Each check carries the step, so a failure says which turn broke.
      const check = (actual: unknown, expected: unknown) =>
        expect({ step: step.say, actual }).toEqual({ step: step.say, actual: expected })

      check(turn.reply, answer.text)
      check(turn.sandbox.harness, step.harness)
      check(request?.model, step.model)
      if (step.effort) check(request?.effort, step.effort)
      if (step.sandbox) check(turn.sandbox.id === turns.at(-1)?.sandbox.id ? 'same' : 'new', step.sandbox)
      check(personaIn(request), step.persona)
      check(noticeIn(turn.context), step.notice)
      for (const flag of step.say.split(' ').filter(word => word.startsWith('-'))) {
        check(request?.userText.includes(flag), false)
      }
      // Only assistant turns prove the harness kept its session; slackbotv2
      // also quotes earlier Slack messages into each turn.
      for (const earlier of step.sees ?? []) {
        check(request?.assistantTurns.includes(`Answer ${earlier}.`), true)
      }
      turns.push(turn)
    }
  }, scenario.steps.length * turnTimeoutMs)
}
