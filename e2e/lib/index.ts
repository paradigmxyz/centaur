// The vocabulary e2e scenarios are written in. Tests act through the same
// doors a user and an operator have: Slack for input and visible output, the
// durable session tables for what the control plane recorded, the scripted
// model server for what the harness actually asked its provider, and api-rs
// and Kubernetes for operator actions and faults.
//
//   const thread = await slack.mention('--codex hi', model.says('Hello.'))
//   const turn = await thread.nextTurn()
//   expect(turn.reply).toBe('Hello.')
import { randomUUID } from 'node:crypto'
import { SQL } from 'bun'
import { BOT, CHANNEL, USER_TOKEN } from '../fakes/slack-fixture'

export { CHANNEL, DEFAULTS_CHANNEL } from '../fakes/slack-fixture'
export type Channel = { id: string; name: string }

/** A scripted model answer, matched by a token placed in the user's message. */
export type Script = { token: string; text: string; delayMs?: number }

export type Execution = { execution_id: string; status: string; error: string | null }

/** A provider request as the model server received it, normalized across providers. */
export type ModelRequest = {
  provider: 'openai' | 'anthropic'
  model?: string
  /** The credential header as the provider received it, after iron-proxy. */
  credential?: string
  /** Reasoning effort the harness asked for. */
  effort?: string
  /**
   * What the user wrote this turn: the last content block of the newest user
   * message. Earlier blocks carry session context and, after a harness
   * switch, the re-fed thread transcript.
   */
  userText: string
  /** Earlier assistant turns in the conversation the model was asked to continue. */
  assistantTurns: string[]
  /** Everything the model was given, as text: system instructions and the whole conversation. */
  prompt: string
  body: any
}

/** The sandbox that ran a turn, and how the control plane obtained it. */
export type Sandbox = { id: string; harness: string; source: string }

export type Turn = {
  /** Text of the turn's one Slack reply. */
  reply: string
  /** Text of the reply's context line: response metadata and notices. */
  context: string
  execution: Execution
  sandbox: Sandbox
  /** The provider request answered by this turn's script, if it had one. */
  request?: ModelRequest
}

type SlackMessage = {
  ts: string
  text?: string
  bot_id?: string
  streaming?: boolean
  blocks?: Array<{ type?: string; elements?: Array<{ text?: string }> }>
}

const slackUrl = required('E2E_SLACK_URL')
const modelUrl = required('E2E_MODEL_URL')
const db = new SQL(required('E2E_DATABASE_URL'))
const apiUrl = required('E2E_API_URL')
const apiKey = required('E2E_API_KEY')
const namespace = 'centaur'
const release = 'centaur'

/** Provider keys iron-proxy holds, as each harness's provider receives them. */
export const providerCredentials: Record<string, string> = {
  codex: `Bearer ${required('E2E_OPENAI_KEY')}`,
  claudecode: required('E2E_ANTHROPIC_KEY')
}
export const turnTimeoutMs = Number(process.env.E2E_TURN_TIMEOUT_MS ?? 300_000)
/** How long a finished execution may take to show up in Slack. */
const renderTimeoutMs = 60_000

export const model = {
  /**
   * Scripts the model's answer to the message this is attached to. `delayMs`
   * holds the answer back, keeping the turn in flight.
   */
  says(text: string, options: { delayMs?: number } = {}): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text, ...options }
  },
  /** Registers a script for a prompt the test sends some other way than Slack. */
  async register(script: Script): Promise<void> {
    await registerScript(script)
  },
  /** Provider requests the script answered, oldest first. */
  async requests(script: Script): Promise<ModelRequest[]> {
    return modelRequests(script.token)
  }
}

export const api = {
  /** Calls api-rs as an operator (the e2e admin key); returns the status and JSON body. */
  async request(method: string, path: string, body?: unknown): Promise<{ status: number; body: any }> {
    const response = await fetch(`${apiUrl}${path}`, {
      method,
      headers: { authorization: `Bearer ${apiKey}`, 'content-type': 'application/json' },
      ...(body === undefined ? {} : { body: JSON.stringify(body) })
    })
    const text = await response.text()
    let payload: unknown = text
    try {
      payload = text ? JSON.parse(text) : undefined
    } catch {
      // Rejections from request extractors are plain text.
    }
    return { status: response.status, body: payload }
  },
  /** Like request, but fails unless api-rs answers 2xx. */
  async ok(method: string, path: string, body?: unknown): Promise<any> {
    const { status, body: payload } = await api.request(method, path, body)
    if (status < 200 || status > 299) throw new Error(`${method} ${path} failed: ${status} ${JSON.stringify(payload)}`)
    return payload
  },
  /** Pauses the thread's sandbox now, as its idle timeout would; returns whether it paused. */
  async pause(thread: Thread): Promise<boolean> {
    const result = await api.ok('POST', `/api/session/${encodeURIComponent(thread.key)}/pause`)
    return result.paused
  }
}

export const cluster = {
  /** Runs a shell script in the api-rs pod, optionally feeding it stdin. */
  async exec(script: string, stdin?: string): Promise<string> {
    return kubectl(
      { stdin },
      'exec', '-i', `deploy/${release}-centaur-api-rs`, '--', 'sh', '-c', script
    )
  },
  /** Kills a sandbox's pod without a graceful shutdown. */
  async killSandbox(sandboxId: string): Promise<void> {
    await kubectl({}, 'delete', 'pod', sandboxId, '--grace-period=0', '--force')
  },
  /** Restarts a release component and waits for the new pods to be ready. */
  async restart(component: string): Promise<void> {
    await kubectl({}, 'rollout', 'restart', `deploy/${release}-centaur-${component}`)
    await kubectl({}, 'rollout', 'status', `deploy/${release}-centaur-${component}`, '--timeout=180s')
  },
  /** Kubernetes objects api-rs created for a sandbox and has not removed. */
  async sandboxResources(sandboxId: string): Promise<string[]> {
    const out = await kubectl(
      {},
      'get', 'sandboxes.agents.x-k8s.io,pods,services,networkpolicies',
      '-l', `centaur.ai/sandbox-id=${sandboxId}`, '-o', 'name'
    )
    return out.split('\n').filter(Boolean)
  }
}

export const slack = {
  /** Starts a thread with a channel message that mentions the bot. */
  async mention(text: string, script?: Script, channel: Channel = CHANNEL): Promise<Thread> {
    const ts = await postMention(channel, text, script)
    return new Thread(channel, ts, script)
  }
}

export class Thread {
  /** The user message that triggered each turn, and that turn's script token. */
  private readonly triggers: string[]
  private readonly scripts: Array<string | undefined>
  private turnsSeen = 0

  constructor(readonly channel: Channel, readonly ts: string, script?: Script) {
    this.triggers = [ts]
    this.scripts = [script?.token]
  }

  get key(): string {
    return `slack:${this.channel.id}:${this.ts}`
  }

  /** Follows up in the thread with a message that mentions the bot. */
  async mention(text: string, script?: Script): Promise<void> {
    this.triggers.push(await postMention(this.channel, text, script, this.ts))
    this.scripts.push(script?.token)
  }

  /**
   * Waits for the next turn: its execution reaches a terminal state and its
   * Slack reply finishes streaming. A turn must produce exactly one reply.
   */
  async nextTurn(): Promise<Turn> {
    const index = this.turnsSeen++
    const trigger = this.triggers[index]
    if (!trigger) throw new Error(`turn ${index + 1} was never triggered in ${this.key}`)

    const execution = await eventually(`execution ${index + 1} of ${this.key}`, turnTimeoutMs, async () => {
      const executions = await this.executions()
      const execution = executions[index]
      return execution && isTerminal(execution.status) ? execution : undefined
    })
    const replies = await eventually(`reply to turn ${index + 1} of ${this.key}`, renderTimeoutMs, async () => {
      const replies = await this.repliesTo(index)
      return replies.length > 0 && replies.every(reply => !reply.streaming) ? replies : undefined
    })
    if (replies.length !== 1) {
      throw new Error(`turn ${index + 1} of ${this.key} produced ${replies.length} replies: ${JSON.stringify(replies)}`)
    }
    const [sandbox]: Sandbox[] = await db`
      select payload->>'sandbox_id' as id, payload->>'harness_type' as harness,
        payload->>'sandbox_ready_source' as source
      from session_events
      where execution_id = ${execution.execution_id} and event_type = 'session.sandbox_ready'
      order by event_id desc limit 1`
    if (!sandbox) throw new Error(`turn ${index + 1} of ${this.key} recorded no ready sandbox`)
    const token = this.scripts[index]
    return {
      reply: replies[0]!.text ?? '',
      context: (replies[0]!.blocks ?? [])
        .filter(block => block.type === 'context')
        .flatMap(block => block.elements ?? [])
        .map(element => element.text ?? '')
        .join(' '),
      execution,
      sandbox,
      request: token ? (await modelRequests(token))[0] : undefined
    }
  }

  /** Sandboxes that served this thread, in the order they became ready. */
  async sandboxes(): Promise<Array<{ id: string; harness: string }>> {
    return db`
      select id, harness from (
        select distinct on (payload->>'sandbox_id') payload->>'sandbox_id' as id,
          payload->>'harness_type' as harness, event_id
        from session_events
        where thread_key = ${this.key} and event_type = 'session.sandbox_ready'
        order by payload->>'sandbox_id', event_id
      ) ready order by event_id`
  }

  private async executions(): Promise<Execution[]> {
    return db`
      select execution_id, status, error from session_executions
      where thread_key = ${this.key} order by created_at`
  }

  /** Bot messages posted after a turn's trigger and before the next user message. */
  private async repliesTo(index: number): Promise<SlackMessage[]> {
    const { messages } = await slackApi<{ messages: SlackMessage[] }>('conversations.replies', {
      channel: this.channel.id,
      ts: this.ts
    })
    const after = messages.filter(message => message.ts > this.triggers[index]!)
    const nextUser = after.find(message => message.bot_id !== BOT.id)
    return after.filter(message => message.bot_id === BOT.id && (!nextUser || message.ts < nextUser.ts))
  }
}

async function postMention(
  channel: Channel,
  text: string,
  script: Script | undefined,
  threadTs?: string
): Promise<string> {
  if (script) await registerScript(script)
  const posted = await slackApi<{ ts: string }>('chat.postMessage', {
    channel: channel.id,
    text: `<@${BOT.userId}> ${text}${script ? ` ${script.token}` : ''}`,
    ...(threadTs ? { thread_ts: threadTs } : {})
  })
  return posted.ts
}

async function registerScript(script: Script): Promise<void> {
  const response = await fetch(`${modelUrl}/_e2e/replies`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ match: script.token, text: script.text, delayMs: script.delayMs })
  })
  if (!response.ok) throw new Error(`registering the model script failed: ${response.status}`)
}

async function modelRequests(token: string): Promise<ModelRequest[]> {
  const response = await fetch(`${modelUrl}/_e2e/requests?match=${encodeURIComponent(token)}`)
  const recorded = (await response.json()) as Array<
    Pick<ModelRequest, 'provider' | 'model' | 'credential' | 'body'> & { conversation: unknown[] }
  >
  return recorded.map(({ conversation, ...request }) => {
    const body = request.body
    const history = conversation as Array<{ role?: string; content?: unknown }>
    return {
      ...request,
      effort: body.reasoning?.effort ?? body.output_config?.effort,
      userText: lastBlockText(history.filter(item => item.role === 'user').at(-1)?.content),
      assistantTurns: history
        .filter(item => item.role === 'assistant')
        .map(item => contentText(item.content)),
      // OpenAI carries system text in `instructions`, Anthropic in `system`.
      prompt: [body.instructions, body.system, ...history.map(item => item.content)]
        .map(contentText)
        .join('\n')
    }
  })
}

function lastBlockText(content: unknown): string {
  return Array.isArray(content) ? contentText(content.slice(-1)) : contentText(content)
}

function contentText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content.map(part => (typeof part?.text === 'string' ? part.text : '')).join('')
}

function isTerminal(status: string): boolean {
  return ['completed', 'failed', 'cancelled'].includes(status)
}

/** Polls until `check` returns a value, or fails naming what it waited for. */
export async function eventually<T>(what: string, timeoutMs: number, check: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const value = await check()
    if (value !== undefined) return value
    await Bun.sleep(1_000)
  }
  throw new Error(`timed out after ${timeoutMs / 1000}s waiting for ${what}`)
}

async function slackApi<T>(method: string, body: Record<string, string>): Promise<T> {
  const response = await fetch(`${slackUrl}/api/${method}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${USER_TOKEN}`, 'content-type': 'application/json' },
    body: JSON.stringify(body)
  })
  const payload = await response.json() as { ok: boolean; error?: string } & T
  if (!payload.ok) throw new Error(`${method} failed: ${payload.error}`)
  return payload
}

async function kubectl(options: { stdin?: string }, ...args: string[]): Promise<string> {
  const proc = Bun.spawn(['kubectl', '-n', namespace, ...args], {
    stdin: options.stdin === undefined ? 'ignore' : new Blob([options.stdin]),
    stdout: 'pipe',
    stderr: 'pipe'
  })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited
  ])
  if (code !== 0) throw new Error(`kubectl ${args.join(' ')} failed: ${stderr.trim()}`)
  return stdout.trim()
}

function required(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required; run the tests with e2e/stack.sh test`)
  return value
}
