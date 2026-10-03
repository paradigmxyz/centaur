// The vocabulary e2e scenarios are written in. Tests act through the same
// doors a user and an operator have: Slack for input and visible output, the
// durable session tables for what the control plane recorded, and the scripted
// model server for what the harness actually asked its provider.
//
//   const thread = await slack.mention('--codex hi', model.says('Hello.'))
//   const turn = await thread.nextTurn()
//   expect(turn.reply).toBe('Hello.')
import { randomUUID } from 'node:crypto'
import { BOT, CHANNEL, USER_TOKEN } from './fixture'

/** A scripted model answer, matched by a token placed in the user's message. */
export type Script = { token: string; text: string }

export type Execution = { execution_id: string; status: string; error: string | null }

/** A provider request as the model server received it, normalized across providers. */
export type ModelRequest = {
  provider: 'openai' | 'anthropic'
  model?: string
  /** The credential header as the provider received it, after iron-proxy. */
  credential?: string
  /** Earlier assistant turns the harness sent back as conversation history. */
  assistantTurns: string[]
  body: any
}

export type Turn = {
  /** Text of the turn's one Slack reply. */
  reply: string
  execution: Execution
  /** The provider request answered by this turn's script, if it had one. */
  request?: ModelRequest
}

type SlackMessage = { ts: string; text?: string; bot_id?: string; streaming?: boolean }

const slackUrl = required('E2E_SLACK_URL')
const modelUrl = required('E2E_MODEL_URL')
const namespace = process.env.E2E_NAMESPACE ?? 'centaur'
const release = process.env.E2E_RELEASE ?? 'centaur'

/** Provider keys iron-proxy holds, as each harness's provider receives them. */
export const providerCredentials: Record<string, string> = {
  codex: `Bearer ${required('E2E_OPENAI_KEY')}`,
  claudecode: required('E2E_ANTHROPIC_KEY')
}
export const turnTimeoutMs = Number(process.env.E2E_TURN_TIMEOUT_MS ?? 300_000)
/** How long a finished execution may take to show up in Slack. */
const renderTimeoutMs = 60_000

export const model = {
  /** Scripts the model's answer to the message this is attached to. */
  says(text: string): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text }
  }
}

export const slack = {
  /** Starts a thread with a channel message that mentions the bot. */
  async mention(text: string, script?: Script): Promise<Thread> {
    const ts = await postMention(text, script)
    return new Thread(ts, script)
  }
}

export class Thread {
  /** The user message that triggered each turn, and that turn's script token. */
  private readonly triggers: string[]
  private readonly scripts: Array<string | undefined>
  private turnsSeen = 0

  constructor(readonly ts: string, script?: Script) {
    this.triggers = [ts]
    this.scripts = [script?.token]
  }

  get key(): string {
    return `slack:${CHANNEL.id}:${this.ts}`
  }

  /** Follows up in the thread with a message that mentions the bot. */
  async mention(text: string, script?: Script): Promise<void> {
    this.triggers.push(await postMention(text, script, this.ts))
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

    const execution = await poll(`execution ${index + 1} of ${this.key}`, turnTimeoutMs, async () => {
      const executions = await this.executions()
      const execution = executions[index]
      return execution && isTerminal(execution.status) ? execution : undefined
    })
    const replies = await poll(`reply to turn ${index + 1} of ${this.key}`, renderTimeoutMs, async () => {
      const replies = await this.repliesTo(index)
      return replies.length > 0 && replies.every(reply => !reply.streaming) ? replies : undefined
    })
    if (replies.length !== 1) {
      throw new Error(`turn ${index + 1} of ${this.key} produced ${replies.length} replies: ${JSON.stringify(replies)}`)
    }
    const token = this.scripts[index]
    return {
      reply: replies[0]!.text ?? '',
      execution,
      request: token ? (await modelRequests(token))[0] : undefined
    }
  }

  /** Sandboxes that served this thread, in the order they became ready. */
  async sandboxes(): Promise<Array<{ id: string; harness: string }>> {
    return sql<{ id: string; harness: string }>(
      `select id, harness from (
         select distinct on (payload->>'sandbox_id') payload->>'sandbox_id' as id,
           payload->>'harness_type' as harness, event_id
         from session_events where thread_key = ${literal(this.key)} and event_type = 'session.sandbox_ready'
         order by payload->>'sandbox_id', event_id
       ) ready order by event_id`
    )
  }

  private async executions(): Promise<Execution[]> {
    return sql<Execution>(
      `select execution_id, status, error from session_executions
       where thread_key = ${literal(this.key)} order by created_at`
    )
  }

  /** Bot messages posted after a turn's trigger and before the next user message. */
  private async repliesTo(index: number): Promise<SlackMessage[]> {
    const { messages } = await slackApi<{ messages: SlackMessage[] }>('conversations.replies', {
      channel: CHANNEL.id,
      ts: this.ts
    })
    const after = messages.filter(message => message.ts > this.triggers[index]!)
    const nextUser = after.find(message => message.bot_id !== BOT.id)
    return after.filter(message => message.bot_id === BOT.id && (!nextUser || message.ts < nextUser.ts))
  }
}

async function postMention(text: string, script: Script | undefined, threadTs?: string): Promise<string> {
  if (script) await registerScript(script)
  const posted = await slackApi<{ ts: string }>('chat.postMessage', {
    channel: CHANNEL.id,
    text: `<@${BOT.userId}> ${text}${script ? ` ${script.token}` : ''}`,
    ...(threadTs ? { thread_ts: threadTs } : {})
  })
  return posted.ts
}

async function registerScript(script: Script): Promise<void> {
  const response = await fetch(`${modelUrl}/_e2e/replies`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ match: script.token, text: script.text })
  })
  if (!response.ok) throw new Error(`registering the model script failed: ${response.status}`)
}

async function modelRequests(token: string): Promise<ModelRequest[]> {
  const response = await fetch(`${modelUrl}/_e2e/requests?match=${encodeURIComponent(token)}`)
  const recorded = (await response.json()) as Array<Omit<ModelRequest, 'assistantTurns'>>
  return recorded.map(request => {
    const history: Array<{ role?: string; content?: unknown }> =
      request.body.input ?? request.body.messages ?? []
    return {
      ...request,
      assistantTurns: history
        .filter(item => item.role === 'assistant')
        .map(item => contentText(item.content))
    }
  })
}

function contentText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content.map(part => (typeof part?.text === 'string' ? part.text : '')).join('')
}

function isTerminal(status: string): boolean {
  return ['completed', 'failed', 'cancelled'].includes(status)
}

/** Polls until `check` returns a value, or fails with what it last saw. */
async function poll<T>(what: string, timeoutMs: number, check: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const value = await check()
    if (value !== undefined) return value
    await Bun.sleep(1_000)
  }
  throw new Error(`timed out after ${timeoutMs / 1000}s waiting for ${what}`)
}

async function sql<T>(query: string): Promise<T[]> {
  const out = await kubectl([
    'exec', `${release}-centaur-postgres-0`, '--', 'sh', '-c',
    'psql -v ON_ERROR_STOP=1 -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "$0"',
    `select coalesce(json_agg(t), '[]') from (${query}) t`
  ])
  return JSON.parse(out) as T[]
}

async function kubectl(args: string[]): Promise<string> {
  const proc = Bun.spawn(['kubectl', '-n', namespace, ...args], { stdout: 'pipe', stderr: 'pipe' })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited
  ])
  if (code !== 0) throw new Error(`kubectl ${args.slice(0, 3).join(' ')} failed: ${stderr.trim()}`)
  return stdout.trim()
}

function literal(value: string): string {
  return `'${value.replaceAll("'", "''")}'`
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

function required(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required; run the tests with e2e/stack.sh test`)
  return value
}
