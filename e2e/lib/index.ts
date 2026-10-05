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
import { BOT, CHANNEL, USER, type SlackUser } from '../fakes/slack-fixture'

export {
  CHANNEL,
  DEFAULTS_CHANNEL,
  EXTERNAL_TEAM,
  EXTERNAL_USER,
  TEAM,
  USER,
  USER_B,
  type SlackUser
} from '../fakes/slack-fixture'
export type Channel = { id: string; name: string }

/** A provider error, as Anthropic's HTTP error or OpenAI's failed response carries it. */
export type ProviderError = { status: number; type: string; code?: string; message: string }

/** A scripted model answer, matched by a token placed in the user's message. */
export type Script = { token: string; text: string; delayMs?: number; error?: ProviderError }

export type Execution = { execution_id: string; status: string; error: string | null }

/** A provider request as the model server received it, normalized across providers. */
export type ModelRequest = {
  provider: 'openai' | 'anthropic'
  model?: string
  /** The credential header as the provider received it, after iron-proxy. */
  credential?: string
  /** The ChatGPT workspace the request was sent for, after iron-proxy. */
  account?: string
  /** Reasoning effort the harness asked for. */
  effort?: string
  /**
   * What the user wrote this turn: the last content block of the newest user
   * message. Earlier blocks carry session context and, after a harness
   * switch, the re-fed thread transcript.
   */
  userText: string
  /** Every text block of the newest user message: the context slackbotv2 added, then what the user wrote. */
  userMessage: string
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

export type SlackMessage = {
  ts: string
  text?: string
  user?: string
  bot_id?: string
  thread_ts?: string
  streaming?: boolean
  blocks?: Array<{ type?: string; elements?: Array<{ text?: string; action_id?: string; value?: string }> }>
  reactions?: Array<{ name: string; users: string[] }>
}

const slackUrl = required('E2E_SLACK_URL')
const modelUrl = required('E2E_MODEL_URL')
const db = new SQL(required('E2E_DATABASE_URL'))
const ironControlDb = new SQL(required('E2E_IRON_CONTROL_DATABASE_URL'))
const apiUrl = required('E2E_API_URL')
const apiKey = required('E2E_API_KEY')
const namespace = 'centaur'
const release = 'centaur'

/**
 * The credential iron-proxy injects for each harness, as its provider receives
 * it: an API key, or a subscription access token in access_token mode.
 */
export const providerCredentials: Record<string, string> = {
  codex: required('E2E_OPENAI_CREDENTIAL'),
  claudecode: required('E2E_ANTHROPIC_CREDENTIAL')
}
/** The ChatGPT workspace iron-proxy routes Codex to; only subscription requests carry one. */
export const chatgptAccountId = process.env.E2E_CHATGPT_ACCOUNT_ID
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
  /** Scripts the provider to fail the request instead of answering it. */
  fails(error: ProviderError): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text: '', error }
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

type MentionOptions = {
  channel?: Channel
  as?: SlackUser
  /** Mentions the bot in an existing thread instead of starting one. */
  threadTs?: string
}

export const slack = {
  /** Starts a thread with a channel message that mentions the bot, by USER unless `as` says otherwise. */
  async mention(text: string, script?: Script, options: MentionOptions = {}): Promise<Thread> {
    const channel = options.channel ?? CHANNEL
    const ts = await postMention(channel, options.as ?? USER, text, script, options.threadTs)
    return new Thread(channel, options.threadTs ?? ts, script, { trigger: ts })
  },
  /** Posts a message that does not mention the bot, starting a thread or replying in one. */
  async post(text: string, options: { channel?: Channel; as?: SlackUser; threadTs?: string } = {}): Promise<string> {
    const posted = await slackApi<{ ts: string }>(options.as ?? USER, 'chat.postMessage', {
      channel: (options.channel ?? CHANNEL).id,
      text,
      ...(options.threadTs ? { thread_ts: options.threadTs } : {})
    })
    return posted.ts
  },
  /** Opens the user's DM with the bot and sends it a message; a DM needs no mention. */
  async dm(text: string, script?: Script, options: { as?: SlackUser } = {}): Promise<Thread> {
    const user = options.as ?? USER
    const { channel } = await slackApi<{ channel: { id: string } }>(user, 'conversations.open', { users: user.id })
    const dm = { id: channel.id, name: `dm-${user.name}` }
    const ts = await postMention(dm, user, text, script, undefined, { dm: true })
    return new Thread(dm, ts, script, { dm: true })
  },
  /** A message as Slack holds it, with its reactions. */
  async message(channel: Channel, ts: string): Promise<SlackMessage> {
    const { message } = await slackApi<{ message: SlackMessage }>(USER, 'reactions.get', { channel: channel.id, timestamp: ts })
    return message
  },
  /** Ephemeral messages a user was shown, oldest first. */
  async ephemeral(user: SlackUser): Promise<Array<{ channel: string; text: string }>> {
    const response = await fetch(`${slackUrl}/_e2e/ephemeral?user=${encodeURIComponent(user.id)}`)
    return response.json()
  },
  /**
   * Clicks a block element on a message, as `as` (USER by default). `value`
   * forges the element's value; `actionTs` repeats an earlier click's delivery.
   */
  async click(
    channel: Channel,
    ts: string,
    actionId: string,
    options: { as?: SlackUser; value?: string; actionTs?: string } = {}
  ): Promise<{ status: number; actionTs: string }> {
    const response = await fetch(`${slackUrl}/_e2e/actions`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        user: (options.as ?? USER).id,
        channel: channel.id,
        message_ts: ts,
        action_id: actionId,
        value: options.value,
        action_ts: options.actionTs
      })
    })
    const result = (await response.json()) as { ok: boolean; error?: string; status: number; action_ts: string }
    if (!result.ok) throw new Error(`clicking ${actionId} failed: ${result.error}`)
    return { status: result.status, actionTs: result.action_ts }
  }
}

export const workflows = {
  /** The payload of the latest workflow event with this name, if one was emitted. */
  async event(name: string): Promise<unknown> {
    const [row] = await db`select payload from absurd.e_centaur_workflows where event_name = ${name}`
    return row?.payload
  }
}

/** A principal as iron-control holds it: who a proxy's credentials act for. */
export type Principal = {
  foreignId: string
  name: string
  kind: string
  slackUserId: string | null
  slackTeamId: string | null
  slackEmail: string | null
}

/**
 * What the control plane registered in iron-control, the source of every
 * sandbox proxy's credentials and grants.
 */
export const ironControl = {
  /** The principals a sandbox's proxy acts for: the conversation, and the requester if one is bound. */
  async proxy(sandboxId: string): Promise<{ principal: Principal; requester: Principal | null }> {
    const [proxy] = await ironControlDb`
      select principal_id, requester_principal_id from proxies where name = ${sandboxId}`
    if (!proxy) throw new Error(`iron-control has no proxy for sandbox ${sandboxId}`)
    const [principal, requester] = await Promise.all(
      [proxy.principal_id, proxy.requester_principal_id].map(async id => {
        if (id === null) return null
        const [row]: Principal[] = await ironControlDb`
          select foreign_id as "foreignId", name, kind, slack_user_id as "slackUserId",
            slack_team_id as "slackTeamId", slack_email as "slackEmail"
          from principals where id = ${id}`
        return row ?? null
      })
    )
    if (!principal) throw new Error(`proxy for sandbox ${sandboxId} has no principal`)
    return { principal, requester: requester ?? null }
  },
  /** The principal for a Slack user, if iron-control has one. */
  async slackUser(user: SlackUser): Promise<Principal | undefined> {
    const [row]: Principal[] = await ironControlDb`
      select foreign_id as "foreignId", name, kind, slack_user_id as "slackUserId",
        slack_team_id as "slackTeamId", slack_email as "slackEmail"
      from principals where slack_user_id = ${user.id}`
    return row
  }
}

/**
 * A conversation with the bot: a channel thread, or a DM, which slackbotv2
 * runs as one conversation-wide session.
 */
export class Thread {
  /** The user message that triggered each turn, and that turn's script token. */
  private readonly triggers: string[]
  private readonly scripts: Array<string | undefined>
  private readonly dm: boolean
  private turnsSeen = 0

  constructor(
    readonly channel: Channel,
    readonly ts: string,
    script?: Script,
    options: { dm?: boolean; trigger?: string } = {}
  ) {
    this.triggers = [options.trigger ?? ts]
    this.scripts = [script?.token]
    this.dm = options.dm ?? false
  }

  get key(): string {
    return this.dm ? `slack:${this.channel.id}:` : `slack:${this.channel.id}:${this.ts}`
  }

  /** Follows up with a message that mentions the bot (in a DM, any message), by USER unless `as` says otherwise. */
  async mention(text: string, script?: Script, options: { as?: SlackUser } = {}): Promise<void> {
    this.triggers.push(await this.send(text, script, options.as))
    this.scripts.push(script?.token)
  }

  /**
   * Mentions the bot while a turn is running. The running turn takes the
   * message (as steering, or a stop), so it starts no turn of its own.
   */
  async interject(text: string, script?: Script, options: { as?: SlackUser } = {}): Promise<string> {
    return this.send(text, script, options.as)
  }

  /** Posts a reply that does not mention the bot. */
  async post(text: string, options: { as?: SlackUser } = {}): Promise<string> {
    return slack.post(text, { channel: this.channel, as: options.as, threadTs: this.dm ? undefined : this.ts })
  }

  /** Waits until the latest turn's model request is in flight, held by its script's delay. */
  async inFlight(): Promise<void> {
    const token = this.scripts.at(-1)
    if (!token) throw new Error(`the latest turn of ${this.key} has no script to wait on`)
    await eventually(`the model request of ${this.key}'s latest turn`, turnTimeoutMs, async () =>
      (await modelRequests(token)).length > 0 ? true : undefined
    )
  }

  private async send(text: string, script: Script | undefined, as: SlackUser = USER): Promise<string> {
    return postMention(this.channel, as, text, script, this.dm ? undefined : this.ts, { dm: this.dm })
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
    // A failure names the execution and the replies so far, so it can be read without the stack.
    const seen = `execution ${JSON.stringify(execution)}`
    let lastReplies: SlackMessage[] = []
    const replies = await eventually(`reply to turn ${index + 1} of ${this.key}`, renderTimeoutMs, async () => {
      lastReplies = await this.repliesTo(index)
      return lastReplies.length > 0 && lastReplies.every(reply => !reply.streaming) ? lastReplies : undefined
    }).catch(error => {
      throw new Error(`${error.message}; ${seen}; replies ${JSON.stringify(lastReplies.map(({ ts, text, streaming }) => ({ ts, text, streaming })))}`)
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
    if (!sandbox) throw new Error(`turn ${index + 1} of ${this.key} recorded no ready sandbox; ${seen}`)
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

  /** Bot messages posted after a turn's trigger and before the next turn's trigger. */
  private async repliesTo(index: number): Promise<SlackMessage[]> {
    const { messages } = this.dm
      ? await slackApi<{ messages: SlackMessage[] }>(USER, 'conversations.history', { channel: this.channel.id })
      : await slackApi<{ messages: SlackMessage[] }>(USER, 'conversations.replies', {
        channel: this.channel.id,
        ts: this.ts
      })
    messages.sort((a, b) => (a.ts < b.ts ? -1 : 1))
    const next = this.triggers[index + 1]
    return messages.filter(message =>
      message.bot_id === BOT.id && message.ts > this.triggers[index]! && (!next || message.ts < next)
    )
  }
}

async function postMention(
  channel: Channel,
  user: SlackUser,
  text: string,
  script: Script | undefined,
  threadTs?: string,
  options: { dm?: boolean } = {}
): Promise<string> {
  if (script) await registerScript(script)
  const posted = await slackApi<{ ts: string }>(user, 'chat.postMessage', {
    channel: channel.id,
    text: `${options.dm ? '' : `<@${BOT.userId}> `}${text}${script ? ` ${script.token}` : ''}`,
    ...(threadTs ? { thread_ts: threadTs } : {})
  })
  return posted.ts
}

async function registerScript(script: Script): Promise<void> {
  const response = await fetch(`${modelUrl}/_e2e/replies`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ match: script.token, text: script.text, delayMs: script.delayMs, error: script.error })
  })
  if (!response.ok) throw new Error(`registering the model script failed: ${response.status}`)
}

async function modelRequests(token: string): Promise<ModelRequest[]> {
  const response = await fetch(`${modelUrl}/_e2e/requests?match=${encodeURIComponent(token)}`)
  const recorded = (await response.json()) as Array<
    Pick<ModelRequest, 'provider' | 'model' | 'credential' | 'account' | 'body'> & { conversation: unknown[] }
  >
  return recorded.map(({ conversation, ...request }) => {
    const body = request.body
    const history = conversation as Array<{ role?: string; content?: unknown }>
    return {
      ...request,
      effort: body.reasoning?.effort ?? body.output_config?.effort,
      userText: lastBlockText(history.filter(item => item.role === 'user').at(-1)?.content),
      userMessage: contentText(history.filter(item => item.role === 'user').at(-1)?.content),
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
  return content.map(part => (typeof part?.text === 'string' ? part.text : '')).join('\n')
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

async function slackApi<T>(user: SlackUser, method: string, body: Record<string, string>): Promise<T> {
  const response = await fetch(`${slackUrl}/api/${method}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${user.token}`, 'content-type': 'application/json' },
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
