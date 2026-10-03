// Helpers for e2e scenarios. Tests act through the same doors a user and an
// operator have: the (fake) Slack API for input and visible output, the
// durable session tables for what the control plane recorded, and the
// scripted model server for what the harness actually asked the provider.
import { randomUUID } from 'node:crypto'
import { BOT, CHANNEL, USER_TOKEN } from './fixture'

export type SlackMessage = { ts: string; text?: string; bot_id?: string; streaming?: boolean }
export type Execution = { execution_id: string; status: string; error: string | null }
export type ModelRequest = {
  provider: 'openai' | 'anthropic'
  match?: string
  model?: string
  /** The credential header as the provider received it. */
  credential?: string
  body: any
}

const slackUrl = required('E2E_SLACK_URL')
const modelUrl = required('E2E_MODEL_URL')
const namespace = process.env.E2E_NAMESPACE ?? 'centaur'
const release = process.env.E2E_RELEASE ?? 'centaur'

/**
 * The provider keys iron-proxy holds, as each harness's provider receives
 * them. Sandboxes only ever hold placeholders.
 */
export const providerCredentials: Record<string, string> = {
  codex: `Bearer ${required('E2E_OPENAI_KEY')}`,
  claudecode: required('E2E_ANTHROPIC_KEY')
}
export const turnTimeoutMs = Number(process.env.E2E_TURN_TIMEOUT_MS ?? 300_000)

export const model = {
  /**
   * Scripts the model's answer to a user message containing the returned
   * token. Put the token in the Slack message the turn answers.
   */
  async reply(text: string): Promise<string> {
    const match = `e2e-${randomUUID().slice(0, 8)}`
    const response = await fetch(`${modelUrl}/_e2e/replies`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ match, text })
    })
    if (!response.ok) throw new Error(`model reply registration failed: ${response.status}`)
    return match
  },
  /** Provider requests the harness made for a token's turn, oldest first. */
  async requests(match: string): Promise<ModelRequest[]> {
    const response = await fetch(`${modelUrl}/_e2e/requests?match=${encodeURIComponent(match)}`)
    return (await response.json()) as ModelRequest[]
  }
}

/** Posts a user message that mentions the bot; returns its ts. */
export async function mention(text: string, threadTs?: string): Promise<string> {
  const posted = await slack<{ ts: string }>('chat.postMessage', {
    channel: CHANNEL.id,
    text: `<@${BOT.userId}> ${text}`,
    ...(threadTs ? { thread_ts: threadTs } : {})
  })
  return posted.ts
}

export function threadKey(rootTs: string): string {
  return `slack:${CHANNEL.id}:${rootTs}`
}

export async function botReplies(rootTs: string): Promise<SlackMessage[]> {
  const thread = await slack<{ messages: SlackMessage[] }>('conversations.replies', {
    channel: CHANNEL.id,
    ts: rootTs
  })
  return thread.messages.filter(message => message.bot_id === BOT.id)
}

/** Waits for a finished (no longer streaming) bot reply in the thread that matches. */
export async function waitForReply(
  rootTs: string,
  matches: (text: string) => boolean,
  timeoutMs = 60_000
): Promise<SlackMessage> {
  let replies: SlackMessage[] = []
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    replies = await botReplies(rootTs)
    const reply = replies.find(message => !message.streaming && matches(message.text ?? ''))
    if (reply) return reply
    await Bun.sleep(1_000)
  }
  throw new Error(`no matching reply in ${threadKey(rootTs)}; bot replies: ${JSON.stringify(replies)}`)
}

/** Waits until the thread has `count` executions and all are terminal. */
export async function waitForExecutions(rootTs: string, count: number): Promise<Execution[]> {
  let executions: Execution[] = []
  const deadline = Date.now() + turnTimeoutMs
  while (Date.now() < deadline) {
    executions = await sql<Execution>(
      `select execution_id, status, error from session_executions
       where thread_key = ${literal(threadKey(rootTs))} order by created_at`
    )
    if (executions.length >= count && executions.every(execution => isTerminal(execution.status))) {
      return executions
    }
    await Bun.sleep(2_000)
  }
  throw new Error(`executions for ${threadKey(rootTs)} did not finish: ${JSON.stringify(executions)}`)
}

function isTerminal(status: string): boolean {
  return ['completed', 'failed', 'cancelled'].includes(status)
}

/** Durable session events for the thread, oldest first. */
export async function sessionEvents(rootTs: string, eventType?: string): Promise<any[]> {
  const rows = await sql<{ payload: unknown }>(
    `select payload from session_events where thread_key = ${literal(threadKey(rootTs))}
     ${eventType ? `and event_type = ${literal(eventType)}` : ''} order by event_id`
  )
  return rows.map(row => row.payload)
}

export async function sql<T>(query: string): Promise<T[]> {
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

async function slack<T>(method: string, body: Record<string, string>): Promise<T> {
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
